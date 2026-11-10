// Alembic (Ogawa) reader for the 3D preview: walks the archive at its first
// sample, bakes transforms into world space and hands each polygon mesh /
// subdivision surface to Rust as triangle corners (positions, normals, UVs).
// The file is opened with a wide path (Alembic's own opener uses ANSI paths).

#include <Alembic/AbcGeom/All.h>
#include <Alembic/AbcCoreOgawa/All.h>
#include <cfloat>
#include <cstdint>
#include <cstring>
#include <fstream>
#include <string>
#include <vector>

using namespace Alembic::AbcGeom;

extern "C" {
typedef struct {
    const char* name;
    uint32_t corners;
    const float* pos;
    const float* nrm;
    const float* uv;
} gt_abc_mesh;

typedef void (*gt_abc_mesh_cb)(void* user, const gt_abc_mesh* m);

typedef struct {
    double start, end, fps;
    uint32_t meshes, subds, xforms, curves, points, cameras, animated, hidden;
    uint64_t max_samples;
    char app[256];
    char version[128];
    char date[64];
    char description[512];
} gt_abc_info;

int gt_abc_read(const wchar_t* path, void* user, gt_abc_mesh_cb cb, gt_abc_info* info, char* err, int errlen);
}

namespace {

struct Ctx {
    gt_abc_mesh_cb cb;
    void* user;
    gt_abc_info* info;
    ISampleSelector sel;
    std::vector<float> pos, nrm, uv;
};

void copy(char* dst, size_t cap, const std::string& s) {
    size_t n = s.size() < cap - 1 ? s.size() : cap - 1;
    memcpy(dst, s.data(), n);
    dst[n] = 0;
}

// Value index for corner `j` (face-varying stream position) of face `f`, point `pi`.
template <class SAMPLE>
bool lookup(const SAMPLE& s, size_t j, size_t f, size_t pi, size_t& out) {
    if (!s.getVals()) return false;
    size_t k;
    switch (s.getScope()) {
        case kFacevaryingScope: k = j; break;
        case kVaryingScope:
        case kVertexScope: k = pi; break;
        case kUniformScope: k = f; break;
        default: k = 0; break;
    }
    if (s.getIndices()) {
        if (k >= s.getIndices()->size()) return false;
        k = (*s.getIndices())[k];
    }
    if (k >= s.getVals()->size()) return false;
    out = k;
    return true;
}

void emit(Ctx& c, const std::string& name, const M44d& world, P3fArraySamplePtr P, Int32ArraySamplePtr idx, Int32ArraySamplePtr cnt,
          IN3fGeomParam* nparam, IV2fGeomParam* uvparam) {
    if (!P || !idx || !cnt || P->size() == 0) return;
    IN3fGeomParam::Sample ns;
    IV2fGeomParam::Sample us;
    bool hasN = false, hasUV = false;
    if (nparam && nparam->valid()) {
        nparam->getIndexed(ns, c.sel);
        hasN = ns.getVals() && ns.getVals()->size() > 0;
    }
    if (uvparam && uvparam->valid()) {
        uvparam->getIndexed(us, c.sel);
        hasUV = us.getVals() && us.getVals()->size() > 0;
    }
    M44d nm = world.inverse().transposed();
    c.pos.clear();
    c.nrm.clear();
    c.uv.clear();
    size_t o = 0;
    const size_t nidx = idx->size();
    for (size_t f = 0; f < cnt->size(); f++) {
        int n = (*cnt)[f];
        if (n < 0 || o + (size_t)n > nidx) break;
        if (n >= 3) {
            // Alembic faces wind clockwise: fan (0, k+1, k) gives counter-clockwise.
            for (int k = 1; k + 1 < n; k++) {
                int local[3] = {0, k + 1, k};
                for (int t = 0; t < 3; t++) {
                    size_t j = o + local[t];
                    size_t pi = (size_t)(*idx)[j];
                    V3f p = pi < P->size() ? (*P)[pi] : V3f(0, 0, 0);
                    V3d w = V3d(p.x, p.y, p.z) * world;
                    c.pos.push_back((float)w.x);
                    c.pos.push_back((float)w.y);
                    c.pos.push_back((float)w.z);
                    if (hasN) {
                        size_t q;
                        V3d v(0, 1, 0);
                        if (lookup(ns, j, f, pi, q)) {
                            N3f nn = (*ns.getVals())[q];
                            nm.multDirMatrix(V3d(nn.x, nn.y, nn.z), v);
                            double l = v.length();
                            if (l > 1e-20) v /= l;
                            else v = V3d(0, 1, 0);
                        }
                        c.nrm.push_back((float)v.x);
                        c.nrm.push_back((float)v.y);
                        c.nrm.push_back((float)v.z);
                    }
                    if (hasUV) {
                        size_t q;
                        V2f t2(0, 0);
                        if (lookup(us, j, f, pi, q)) t2 = (*us.getVals())[q];
                        c.uv.push_back(t2.x);
                        c.uv.push_back(1.0f - t2.y);
                    }
                }
            }
        }
        o += (size_t)n;
    }
    if (c.pos.empty()) return;
    gt_abc_mesh m;
    m.name = name.c_str();
    m.corners = (uint32_t)(c.pos.size() / 3);
    m.pos = c.pos.data();
    m.nrm = hasN ? c.nrm.data() : nullptr;
    m.uv = hasUV ? c.uv.data() : nullptr;
    c.cb(c.user, &m);
}

bool hidden(IObject& o, const ISampleSelector& sel) {
    IVisibilityProperty v = GetVisibilityProperty(o);
    if (!v.valid()) return false;
    return v.getValue(sel) == (int8_t)kVisibilityHidden;
}

void walk(Ctx& c, IObject parent, const M44d& xf) {
    for (size_t i = 0; i < parent.getNumChildren(); i++) {
        IObject child = parent.getChild(i);
        if (!child.valid()) continue;
        if (hidden(child, c.sel)) {
            c.info->hidden++;
            continue;
        }
        const MetaData& md = child.getMetaData();
        if (IXform::matches(md)) {
            IXform x(child, kWrapExisting);
            XformSample s;
            x.getSchema().get(s, c.sel);
            M44d local = s.getMatrix();
            M44d world = s.getInheritsXforms() ? local * xf : local;
            c.info->xforms++;
            if (!x.getSchema().isConstant()) c.info->animated++;
            walk(c, x, world);
        } else if (IPolyMesh::matches(md)) {
            IPolyMesh pm(child, kWrapExisting);
            IPolyMeshSchema& s = pm.getSchema();
            IPolyMeshSchema::Sample smp;
            s.get(smp, c.sel);
            c.info->meshes++;
            if (!s.isConstant()) c.info->animated++;
            if (s.getNumSamples() > c.info->max_samples) c.info->max_samples = s.getNumSamples();
            IN3fGeomParam n = s.getNormalsParam();
            IV2fGeomParam uv = s.getUVsParam();
            emit(c, child.getFullName(), xf, smp.getPositions(), smp.getFaceIndices(), smp.getFaceCounts(), &n, &uv);
            walk(c, child, xf);
        } else if (ISubD::matches(md)) {
            ISubD sd(child, kWrapExisting);
            ISubDSchema& s = sd.getSchema();
            ISubDSchema::Sample smp;
            s.get(smp, c.sel);
            c.info->subds++;
            if (!s.isConstant()) c.info->animated++;
            if (s.getNumSamples() > c.info->max_samples) c.info->max_samples = s.getNumSamples();
            IV2fGeomParam uv = s.getUVsParam();
            emit(c, child.getFullName(), xf, smp.getPositions(), smp.getFaceIndices(), smp.getFaceCounts(), nullptr, &uv);
            walk(c, child, xf);
        } else {
            if (ICurves::matches(md)) c.info->curves++;
            else if (IPoints::matches(md)) c.info->points++;
            else if (ICamera::matches(md)) c.info->cameras++;
            walk(c, child, xf);
        }
    }
}

}  // namespace

int gt_abc_read(const wchar_t* path, void* user, gt_abc_mesh_cb cb, gt_abc_info* info, char* err, int errlen) {
    try {
        std::ifstream f(path, std::ios::in | std::ios::binary);
        if (!f) {
            copy(err, errlen, "cannot open the file");
            return 0;
        }
        std::vector<std::istream*> streams{&f};
        IArchive archive(Alembic::AbcCoreOgawa::ReadArchive(streams), "archive.abc", ErrorHandler::kThrowPolicy);
        if (!archive.valid()) {
            copy(err, errlen, "not a valid Alembic archive");
            return 0;
        }
        std::string app, ver, date, desc;
        Alembic::Util::uint32_t api = 0;
        double dcc = 0;
        GetArchiveInfo(archive, app, ver, api, date, desc, dcc);
        copy(info->app, sizeof(info->app), app);
        copy(info->version, sizeof(info->version), ver);
        copy(info->date, sizeof(info->date), date);
        copy(info->description, sizeof(info->description), desc);
        double start = DBL_MAX, end = -DBL_MAX;
        GetArchiveStartAndEndTime(archive, start, end);
        info->start = start;
        info->end = end;
        info->fps = dcc;
        if (info->fps <= 0 && archive.getNumTimeSamplings() > 1) {
            AbcA::TimeSamplingPtr ts = archive.getTimeSampling(1);
            double tpc = ts->getTimeSamplingType().getTimePerCycle();
            if (ts->getTimeSamplingType().isUniform() && tpc > 0) info->fps = 1.0 / tpc;
        }
        Ctx c{cb, user, info, start <= end ? ISampleSelector(start) : ISampleSelector((index_t)0), {}, {}, {}};
        M44d id;
        id.makeIdentity();
        walk(c, archive.getTop(), id);
        return 1;
    } catch (std::exception& e) {
        copy(err, errlen, e.what());
    } catch (...) {
        copy(err, errlen, "unknown error");
    }
    return 0;
}
