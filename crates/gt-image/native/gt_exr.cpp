// OpenEXR decoder: first part, RGBA view (luminance/chroma files are converted by
// the library), half floats, plus the chromaticities attribute when present.
#include "gt_codecs.h"

#include <ImfArray.h>
#include <ImfChromaticitiesAttribute.h>
#include <ImfHeader.h>
#include <ImfRgbaFile.h>
#include <ImfStandardAttributes.h>
#include <ImfThreading.h>

#include <cstdlib>
#include <cstring>
#include <exception>
#include <mutex>

static std::once_flag g_threads;

extern "C" int gt_exr_decode(const char* utf8_path, gt_img* o) {
    memset(o, 0, sizeof(*o));
    o->cicp_primaries = o->cicp_transfer = o->cicp_matrix = o->cicp_full = -1;
    o->orientation = 1;
    o->frames = 1;
    try {
        std::call_once(g_threads, [] { Imf::setGlobalThreadCount(8); });
        Imf::RgbaInputFile f(utf8_path);
        const Imath::Box2i dw = f.dataWindow();
        const int w = dw.max.x - dw.min.x + 1, h = dw.max.y - dw.min.y + 1;
        if (w <= 0 || h <= 0) throw std::runtime_error("empty data window");
        Imf::Array2D<Imf::Rgba> px(h, w);
        f.setFrameBuffer(&px[0][0] - dw.min.x - static_cast<ptrdiff_t>(dw.min.y) * w, 1, w);
        f.readPixels(dw.min.y, dw.max.y);
        o->pixels = static_cast<uint8_t*>(malloc(static_cast<size_t>(w) * h * 8));
        if (!o->pixels) throw std::bad_alloc();
        memcpy(o->pixels, &px[0][0], static_cast<size_t>(w) * h * 8);
        o->width = w;
        o->height = h;
        o->stride = w * 8;
        o->format = GT_RGBAF16;
        o->bit_depth = 16;
        o->has_alpha = (f.channels() & Imf::WRITE_A) != 0;
        o->cicp_transfer = 8; /* linear */
        if (Imf::hasChromaticities(f.header())) {
            const Imf::Chromaticities& c = Imf::chromaticities(f.header());
            o->has_prim = 1;
            const float v[8] = {c.red.x, c.red.y, c.green.x, c.green.y, c.blue.x, c.blue.y, c.white.x, c.white.y};
            memcpy(o->prim, v, sizeof(v));
        } else {
            o->cicp_primaries = 1;
        }
        return 0;
    } catch (const std::exception& e) {
        free(o->pixels);
        o->pixels = nullptr;
        strncpy(o->error, e.what(), sizeof(o->error) - 1);
        return -1;
    }
}
