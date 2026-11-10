/* Native decoders: libavif, libheif, libjxl, libwebp, libjpeg-turbo, libtiff,
   OpenJPEG and LibRaw behind the gt_img ABI (see gt_codecs.h). */
#define LIBRAW_NODLL
#define _CRT_SECURE_NO_WARNINGS
#include "gt_codecs.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <windows.h>

#include <avif/avif.h>
#include <jxl/decode.h>
#include <jxl/thread_parallel_runner.h>
#include <libheif/heif.h>
#include <libheif/heif_properties.h>
#include <libraw/libraw.h>
#include <openjpeg.h>
#include <tiffio.h>
#include <turbojpeg.h>
#include <webp/decode.h>
#include <webp/demux.h>

/* ------------------------------------------------------------------ helpers */

static void set_err(gt_img* o, const char* fmt, const char* detail) {
    if (o) _snprintf(o->error, sizeof(o->error) - 1, fmt, detail ? detail : "");
}

static void init_img(gt_img* o) {
    memset(o, 0, sizeof(*o));
    o->cicp_primaries = o->cicp_transfer = o->cicp_matrix = o->cicp_full = -1;
    o->orientation = 1;
    o->frames = 1;
}

static uint8_t* dup_bytes(const uint8_t* p, size_t n) {
    if (!p || !n) return NULL;
    uint8_t* d = (uint8_t*)malloc(n);
    if (d) memcpy(d, p, n);
    return d;
}

static int cpu_threads(void) {
    SYSTEM_INFO si;
    GetSystemInfo(&si);
    int n = (int)si.dwNumberOfProcessors;
    return n < 1 ? 1 : (n > 16 ? 16 : n);
}

void gt_free_buf(void* p) { free(p); }

void gt_free(gt_img* img) {
    if (!img) return;
    free(img->pixels);
    free(img->icc);
    free(img->exif);
    free(img->xmp);
    if (img->gainmap) {
        gt_free(img->gainmap);
        free(img->gainmap);
    }
    img->pixels = img->icc = img->exif = img->xmp = NULL;
    img->gainmap = NULL;
}

/* Compose rotation (CCW quarter turns) and mirroring into an EXIF orientation.
   State (r, m): display = rotate_cw(r) o mirror_h^m. */
typedef struct { int r, m; } orient_state;
static void o_rot_ccw(orient_state* s, int quarters) { s->r = (s->r + 4 - (quarters & 3)) & 3; }
static void o_flip_h(orient_state* s) { s->r = (4 - s->r) & 3; s->m ^= 1; }
static void o_flip_v(orient_state* s) { o_flip_h(s); s->r = (s->r + 2) & 3; }
static int o_exif(orient_state s) {
    static const int map[4][2] = {{1, 2}, {6, 7}, {3, 4}, {8, 5}};
    return map[s.r & 3][s.m & 1];
}

/* Expand 8-bit gray or RGB into RGBA. */
static uint8_t* expand_rgba8(const uint8_t* src, int w, int h, size_t stride, int channels) {
    uint8_t* out = (uint8_t*)malloc((size_t)w * h * 4);
    if (!out) return NULL;
    for (int y = 0; y < h; y++) {
        const uint8_t* s = src + (size_t)y * stride;
        uint8_t* d = out + (size_t)y * w * 4;
        for (int x = 0; x < w; x++) {
            if (channels == 1) { d[0] = d[1] = d[2] = s[x]; d[3] = 255; }
            else if (channels == 2) { d[0] = d[1] = d[2] = s[2 * x]; d[3] = s[2 * x + 1]; }
            else if (channels == 3) { d[0] = s[3 * x]; d[1] = s[3 * x + 1]; d[2] = s[3 * x + 2]; d[3] = 255; }
            else { memcpy(d, s + 4 * x, 4); }
            d += 4;
        }
    }
    return out;
}

/* ------------------------------------------------------------------ AVIF */

static float sfrac(avifSignedFraction f) { return f.d ? (float)f.n / (float)f.d : 0.0f; }
static float ufrac(avifUnsignedFraction f) { return f.d ? (float)f.n / (float)f.d : 0.0f; }

static int avif_to_rgb(avifImage* img, gt_img* o) {
    avifRGBImage rgb;
    avifRGBImageSetDefaults(&rgb, img);
    rgb.format = AVIF_RGB_FORMAT_RGBA;
    rgb.depth = img->depth > 8 ? 16 : 8;
    rgb.maxThreads = cpu_threads();
    if (avifRGBImageAllocatePixels(&rgb) != AVIF_RESULT_OK) return -1;
    if (avifImageYUVToRGB(img, &rgb) != AVIF_RESULT_OK) {
        avifRGBImageFreePixels(&rgb);
        return -1;
    }
    o->width = (int32_t)rgb.width;
    o->height = (int32_t)rgb.height;
    o->format = rgb.depth == 16 ? GT_RGBA16 : GT_RGBA8;
    o->stride = (int32_t)(rgb.width * (rgb.depth == 16 ? 8 : 4));
    size_t n = (size_t)o->stride * o->height;
    o->pixels = (uint8_t*)malloc(n);
    if (o->pixels) {
        for (uint32_t y = 0; y < rgb.height; y++) memcpy(o->pixels + (size_t)y * o->stride, rgb.pixels + (size_t)y * rgb.rowBytes, o->stride);
    }
    o->bit_depth = (int32_t)img->depth;
    avifRGBImageFreePixels(&rgb);
    return o->pixels ? 0 : -1;
}

int gt_avif_decode(const uint8_t* data, size_t len, gt_img* o) {
    init_img(o);
    avifDecoder* dec = avifDecoderCreate();
    if (!dec) { set_err(o, "avif: out of memory%s", NULL); return -1; }
    dec->maxThreads = cpu_threads();
    dec->imageContentToDecode = AVIF_IMAGE_CONTENT_ALL;
    dec->strictFlags = AVIF_STRICT_DISABLED;
    avifResult r = avifDecoderSetIOMemory(dec, data, len);
    if (r == AVIF_RESULT_OK) r = avifDecoderParse(dec);
    if (r == AVIF_RESULT_OK) r = avifDecoderNextImage(dec);
    if (r != AVIF_RESULT_OK) {
        set_err(o, "avif: %s", avifResultToString(r));
        avifDecoderDestroy(dec);
        return -1;
    }
    avifImage* img = dec->image;
    o->frames = dec->imageCount;
    o->delay_ms = (int32_t)(dec->imageTiming.duration * 1000.0);
    o->cicp_primaries = img->colorPrimaries;
    o->cicp_transfer = img->transferCharacteristics;
    o->cicp_matrix = img->matrixCoefficients;
    o->cicp_full = img->yuvRange == AVIF_RANGE_FULL;
    o->has_alpha = img->alphaPlane != NULL;
    o->icc = dup_bytes(img->icc.data, img->icc.size);
    o->icc_len = (int32_t)img->icc.size;
    o->exif = dup_bytes(img->exif.data, img->exif.size);
    o->exif_len = (int32_t)img->exif.size;
    o->xmp = dup_bytes(img->xmp.data, img->xmp.size);
    o->xmp_len = (int32_t)img->xmp.size;
    o->max_cll = img->clli.maxCLL;
    o->max_fall = img->clli.maxPALL;
    orient_state s = {0, 0};
    if (img->transformFlags & AVIF_TRANSFORM_IROT) o_rot_ccw(&s, img->irot.angle);
    if (img->transformFlags & AVIF_TRANSFORM_IMIR) {
        if (img->imir.axis == 0) o_flip_v(&s); else o_flip_h(&s);
    }
    o->orientation = o_exif(s);
    if (avif_to_rgb(img, o) != 0) {
        set_err(o, "avif: RGB conversion failed%s", NULL);
        avifDecoderDestroy(dec);
        gt_free(o);
        return -1;
    }
    avifGainMap* gm = img->gainMap;
    if (gm && gm->image) {
        gt_img* g = (gt_img*)calloc(1, sizeof(gt_img));
        if (g) {
            init_img(g);
            if (avif_to_rgb(gm->image, g) == 0) {
                g->cicp_transfer = gm->image->transferCharacteristics;
                o->gainmap = g;
                o->gm_kind = GT_GM_ISO;
                for (int c = 0; c < 3; c++) {
                    o->gm_min[c] = sfrac(gm->gainMapMin[c]);
                    o->gm_max[c] = sfrac(gm->gainMapMax[c]);
                    o->gm_gamma[c] = ufrac(gm->gainMapGamma[c]);
                    o->gm_base_offset[c] = sfrac(gm->baseOffset[c]);
                    o->gm_alt_offset[c] = sfrac(gm->alternateOffset[c]);
                }
                o->gm_base_headroom = ufrac(gm->baseHdrHeadroom);
                o->gm_alt_headroom = ufrac(gm->alternateHdrHeadroom);
                o->gm_use_base_cs = gm->useBaseColorSpace;
            } else {
                free(g);
            }
        }
    }
    avifDecoderDestroy(dec);
    return 0;
}

/* ------------------------------------------------------------------ HEIF */

static int heif_decode_handle(heif_context* ctx, heif_image_handle* h, gt_img* o, int mono) {
    (void)ctx;
    int has_alpha = !mono && heif_image_handle_has_alpha_channel(h);
    int bits = heif_image_handle_get_luma_bits_per_pixel(h);
    heif_decoding_options* opts = heif_decoding_options_alloc();
    opts->ignore_transformations = 1; /* orientation is applied at render time */
    heif_image* img = NULL;
    heif_chroma chroma;
    heif_colorspace cs = heif_colorspace_RGB;
    if (mono) { cs = heif_colorspace_monochrome; chroma = heif_chroma_monochrome; }
    else if (bits > 8) chroma = has_alpha ? heif_chroma_interleaved_RRGGBBAA_LE : heif_chroma_interleaved_RRGGBB_LE;
    else chroma = has_alpha ? heif_chroma_interleaved_RGBA : heif_chroma_interleaved_RGB;
    heif_error e = heif_decode_image(h, &img, cs, chroma, opts);
    heif_decoding_options_free(opts);
    if (e.code != heif_error_Ok) {
        set_err(o, "heif: %s", e.message);
        return -1;
    }
    heif_channel ch = mono ? heif_channel_Y : heif_channel_interleaved;
    size_t stride = 0;
    const uint8_t* p = heif_image_get_plane_readonly2(img, ch, &stride);
    int w = heif_image_get_width(img, ch), hh = heif_image_get_height(img, ch);
    if (!p || w <= 0 || hh <= 0) {
        heif_image_release(img);
        set_err(o, "heif: no pixel data%s", NULL);
        return -1;
    }
    o->width = w;
    o->height = hh;
    o->has_alpha = has_alpha;
    o->bit_depth = bits;
    if (!mono && bits > 8) {
        int ch_n = has_alpha ? 4 : 3;
        int shift_l = 16 - bits, shift_r = bits - shift_l;
        uint16_t* out = (uint16_t*)malloc((size_t)w * hh * 8);
        if (out) {
            for (int y = 0; y < hh; y++) {
                const uint16_t* s = (const uint16_t*)(p + (size_t)y * stride);
                uint16_t* d = out + (size_t)y * w * 4;
                for (int x = 0; x < w; x++) {
                    for (int c = 0; c < 3; c++) {
                        uint16_t v = s[x * ch_n + c];
                        d[c] = (uint16_t)((v << shift_l) | (shift_r > 0 ? (v >> shift_r) : 0));
                    }
                    if (has_alpha) { uint16_t v = s[x * ch_n + 3]; d[3] = (uint16_t)((v << shift_l) | (shift_r > 0 ? (v >> shift_r) : 0)); }
                    else d[3] = 65535;
                    d += 4;
                }
            }
        }
        o->pixels = (uint8_t*)out;
        o->format = GT_RGBA16;
        o->stride = w * 8;
    } else {
        o->pixels = expand_rgba8(p, w, hh, stride, mono ? 1 : (has_alpha ? 4 : 3));
        o->format = GT_RGBA8;
        o->stride = w * 4;
    }
    heif_image_release(img);
    return o->pixels ? 0 : -1;
}

static void heif_metadata(heif_image_handle* h, const char* type, uint8_t** out, int32_t* out_len) {
    heif_item_id ids[8];
    int n = heif_image_handle_get_list_of_metadata_block_IDs(h, type, ids, 8);
    for (int i = 0; i < n; i++) {
        if (strcmp(type, "mime") == 0) {
            const char* ct = heif_image_handle_get_metadata_content_type(h, ids[i]);
            if (!ct || strcmp(ct, "application/rdf+xml") != 0) continue;
        }
        size_t sz = heif_image_handle_get_metadata_size(h, ids[i]);
        if (!sz) continue;
        uint8_t* buf = (uint8_t*)malloc(sz);
        if (!buf) return;
        if (heif_image_handle_get_metadata(h, ids[i], buf).code == heif_error_Ok) {
            *out = buf;
            *out_len = (int32_t)sz;
            return;
        }
        free(buf);
    }
}

int gt_heif_decode(const uint8_t* data, size_t len, gt_img* o) {
    init_img(o);
    heif_context* ctx = heif_context_alloc();
    heif_error e = heif_context_read_from_memory_without_copy(ctx, data, len, NULL);
    heif_image_handle* h = NULL;
    if (e.code == heif_error_Ok) e = heif_context_get_primary_image_handle(ctx, &h);
    if (e.code != heif_error_Ok) {
        set_err(o, "heif: %s", e.message);
        heif_context_free(ctx);
        return -1;
    }
    o->frames = heif_context_get_number_of_top_level_images(ctx);
    if (heif_decode_handle(ctx, h, o, 0) != 0) {
        heif_image_handle_release(h);
        heif_context_free(ctx);
        return -1;
    }
    /* Orientation from irot/imir, applied in order. */
    heif_item_id item = heif_image_handle_get_item_id(h);
    heif_property_id props[8];
    int np = heif_item_get_transformation_properties(ctx, item, props, 8);
    orient_state s = {0, 0};
    for (int i = 0; i < np; i++) {
        enum heif_item_property_type t = heif_item_get_property_type(ctx, item, props[i]);
        if (t == heif_item_property_type_transform_rotation) {
            int deg = heif_item_get_property_transform_rotation_ccw(ctx, item, props[i]);
            if (deg > 0) o_rot_ccw(&s, deg / 90);
        } else if (t == heif_item_property_type_transform_mirror) {
            enum heif_transform_mirror_direction d = heif_item_get_property_transform_mirror(ctx, item, props[i]);
            if (d == heif_transform_mirror_direction_vertical) o_flip_v(&s);
            else if (d == heif_transform_mirror_direction_horizontal) o_flip_h(&s);
        }
    }
    o->orientation = o_exif(s);
    /* Colour: nclx and/or ICC. */
    heif_color_profile_nclx* nclx = NULL;
    if (heif_image_handle_get_nclx_color_profile(h, &nclx).code == heif_error_Ok && nclx) {
        o->cicp_primaries = nclx->color_primaries;
        o->cicp_transfer = nclx->transfer_characteristics;
        o->cicp_matrix = nclx->matrix_coefficients;
        o->cicp_full = nclx->full_range_flag;
        heif_nclx_color_profile_free(nclx);
    }
    size_t icc_n = heif_image_handle_get_raw_color_profile_size(h);
    if (icc_n) {
        o->icc = (uint8_t*)malloc(icc_n);
        if (o->icc && heif_image_handle_get_raw_color_profile(h, o->icc).code == heif_error_Ok) o->icc_len = (int32_t)icc_n;
        else { free(o->icc); o->icc = NULL; }
    }
    heif_content_light_level cll;
    if (heif_image_handle_get_content_light_level(h, &cll)) {
        o->max_cll = cll.max_content_light_level;
        o->max_fall = cll.max_pic_average_light_level;
    }
    heif_mastering_display_colour_volume mdcv;
    if (heif_image_handle_get_mastering_display_colour_volume(h, &mdcv)) {
        heif_decoded_mastering_display_colour_volume dm;
        if (heif_mastering_display_colour_volume_decode(&mdcv, &dm).code == heif_error_Ok) o->mastering_max = (float)dm.max_display_mastering_luminance;
    }
    heif_metadata(h, "Exif", &o->exif, &o->exif_len);
    heif_metadata(h, "mime", &o->xmp, &o->xmp_len);
    /* Apple HDR gain map: an auxiliary image in the primary's (untransformed) frame. */
    int naux = heif_image_handle_get_number_of_auxiliary_images(h, LIBHEIF_AUX_IMAGE_FILTER_OMIT_ALPHA | LIBHEIF_AUX_IMAGE_FILTER_OMIT_DEPTH);
    if (naux > 0) {
        heif_item_id aux_ids[8];
        int n = heif_image_handle_get_list_of_auxiliary_image_IDs(h, LIBHEIF_AUX_IMAGE_FILTER_OMIT_ALPHA | LIBHEIF_AUX_IMAGE_FILTER_OMIT_DEPTH, aux_ids, 8);
        for (int i = 0; i < n && !o->gainmap; i++) {
            heif_image_handle* ah = NULL;
            if (heif_image_handle_get_auxiliary_image_handle(h, aux_ids[i], &ah).code != heif_error_Ok) continue;
            const char* type = NULL;
            if (heif_image_handle_get_auxiliary_type(ah, &type).code == heif_error_Ok && type) {
                if (strstr(type, "hdrgainmap")) {
                    gt_img* g = (gt_img*)calloc(1, sizeof(gt_img));
                    if (g) {
                        init_img(g);
                        if (heif_decode_handle(ctx, ah, g, 1) == 0) {
                            o->gainmap = g;
                            o->gm_kind = GT_GM_APPLE;
                        } else {
                            free(g);
                        }
                    }
                }
                heif_image_handle_release_auxiliary_type(ah, &type);
            }
            heif_image_handle_release(ah);
        }
    }
    heif_image_handle_release(h);
    heif_context_free(ctx);
    return 0;
}

/* ------------------------------------------------------------------ JPEG XL */

static int jxl_primaries_cicp(JxlPrimaries p) {
    switch (p) {
    case JXL_PRIMARIES_SRGB: return 1;
    case JXL_PRIMARIES_2100: return 9;
    case JXL_PRIMARIES_P3: return 12;
    default: return -1;
    }
}

int gt_jxl_decode(const uint8_t* data, size_t len, gt_img* o) {
    init_img(o);
    JxlDecoder* dec = JxlDecoderCreate(NULL);
    void* runner = JxlThreadParallelRunnerCreate(NULL, JxlThreadParallelRunnerDefaultNumWorkerThreads());
    if (!dec || !runner) { set_err(o, "jxl: out of memory%s", NULL); goto fail; }
    if (JxlDecoderSetParallelRunner(dec, JxlThreadParallelRunner, runner) != JXL_DEC_SUCCESS) goto fail;
    if (JxlDecoderSubscribeEvents(dec, JXL_DEC_BASIC_INFO | JXL_DEC_COLOR_ENCODING | JXL_DEC_FRAME | JXL_DEC_FULL_IMAGE) != JXL_DEC_SUCCESS) goto fail;
    JxlDecoderSetInput(dec, data, len);
    JxlDecoderCloseInput(dec);
    JxlBasicInfo info;
    memset(&info, 0, sizeof(info));
    JxlPixelFormat fmt = {4, JXL_TYPE_UINT8, JXL_NATIVE_ENDIAN, 0};
    int hdr = 0;
    for (;;) {
        JxlDecoderStatus st = JxlDecoderProcessInput(dec);
        if (st == JXL_DEC_ERROR) { set_err(o, "jxl: decode error%s", NULL); goto fail; }
        if (st == JXL_DEC_NEED_MORE_INPUT) { set_err(o, "jxl: truncated file%s", NULL); goto fail; }
        if (st == JXL_DEC_BASIC_INFO) {
            if (JxlDecoderGetBasicInfo(dec, &info) != JXL_DEC_SUCCESS) goto fail;
            o->width = (int32_t)info.xsize;
            o->height = (int32_t)info.ysize;
            o->has_alpha = info.alpha_bits > 0;
            o->bit_depth = (int32_t)info.bits_per_sample;
            o->intensity_target = info.intensity_target;
            o->frames = info.have_animation ? 2 : 1;
        } else if (st == JXL_DEC_COLOR_ENCODING) {
            JxlColorEncoding enc;
            if (JxlDecoderGetColorAsEncodedProfile(dec, JXL_COLOR_PROFILE_TARGET_DATA, &enc) == JXL_DEC_SUCCESS) {
                if (enc.color_space == JXL_COLOR_SPACE_GRAY) o->cicp_primaries = 1;
                else {
                    o->cicp_primaries = jxl_primaries_cicp(enc.primaries);
                    if (o->cicp_primaries < 0) {
                        o->has_prim = 1;
                        o->prim[0] = (float)enc.primaries_red_xy[0]; o->prim[1] = (float)enc.primaries_red_xy[1];
                        o->prim[2] = (float)enc.primaries_green_xy[0]; o->prim[3] = (float)enc.primaries_green_xy[1];
                        o->prim[4] = (float)enc.primaries_blue_xy[0]; o->prim[5] = (float)enc.primaries_blue_xy[1];
                        o->prim[6] = (float)enc.white_point_xy[0]; o->prim[7] = (float)enc.white_point_xy[1];
                    }
                }
                switch (enc.transfer_function) {
                case JXL_TRANSFER_FUNCTION_709: o->cicp_transfer = 1; break;
                case JXL_TRANSFER_FUNCTION_LINEAR: o->cicp_transfer = 8; hdr = 1; break;
                case JXL_TRANSFER_FUNCTION_SRGB: o->cicp_transfer = 13; break;
                case JXL_TRANSFER_FUNCTION_PQ: o->cicp_transfer = 16; hdr = 1; break;
                case JXL_TRANSFER_FUNCTION_DCI: o->cicp_transfer = 17; break;
                case JXL_TRANSFER_FUNCTION_HLG: o->cicp_transfer = 18; hdr = 1; break;
                case JXL_TRANSFER_FUNCTION_GAMMA: o->cicp_transfer = 4; o->gamma = (float)(1.0 / enc.gamma); break;
                default: o->cicp_transfer = 13; break;
                }
            } else {
                size_t n = 0;
                if (JxlDecoderGetICCProfileSize(dec, JXL_COLOR_PROFILE_TARGET_DATA, &n) == JXL_DEC_SUCCESS && n) {
                    o->icc = (uint8_t*)malloc(n);
                    if (o->icc && JxlDecoderGetColorAsICCProfile(dec, JXL_COLOR_PROFILE_TARGET_DATA, o->icc, n) == JXL_DEC_SUCCESS) o->icc_len = (int32_t)n;
                    else { free(o->icc); o->icc = NULL; }
                }
            }
            if (hdr || info.bits_per_sample > 8 || info.exponent_bits_per_sample > 0) fmt.data_type = JXL_TYPE_FLOAT16;
        } else if (st == JXL_DEC_FRAME) {
            JxlFrameHeader fh;
            if (JxlDecoderGetFrameHeader(dec, &fh) == JXL_DEC_SUCCESS && info.have_animation && info.animation.tps_numerator) {
                o->delay_ms = (int32_t)(1000.0 * fh.duration * info.animation.tps_denominator / info.animation.tps_numerator);
            }
        } else if (st == JXL_DEC_NEED_IMAGE_OUT_BUFFER) {
            size_t n = 0;
            if (JxlDecoderImageOutBufferSize(dec, &fmt, &n) != JXL_DEC_SUCCESS) goto fail;
            o->pixels = (uint8_t*)malloc(n);
            if (!o->pixels) goto fail;
            if (JxlDecoderSetImageOutBuffer(dec, &fmt, o->pixels, n) != JXL_DEC_SUCCESS) goto fail;
            o->format = fmt.data_type == JXL_TYPE_FLOAT16 ? GT_RGBAF16 : GT_RGBA8;
            o->stride = o->width * (fmt.data_type == JXL_TYPE_FLOAT16 ? 8 : 4);
        } else if (st == JXL_DEC_FULL_IMAGE) {
            break; /* first frame is enough for a preview */
        } else if (st == JXL_DEC_SUCCESS) {
            break;
        }
    }
    JxlThreadParallelRunnerDestroy(runner);
    JxlDecoderDestroy(dec);
    if (!o->pixels) { set_err(o, "jxl: no image%s", NULL); gt_free(o); return -1; }
    return 0;
fail:
    if (!o->error[0]) set_err(o, "jxl: decoder setup failed%s", NULL);
    if (runner) JxlThreadParallelRunnerDestroy(runner);
    if (dec) JxlDecoderDestroy(dec);
    gt_free(o);
    return -1;
}

/* ------------------------------------------------------------------ WebP */

static void webp_chunks(const uint8_t* data, size_t len, gt_img* o) {
    WebPData wd = {data, len};
    WebPDemuxer* demux = WebPDemux(&wd);
    if (!demux) return;
    WebPChunkIterator it;
    if (WebPDemuxGetChunk(demux, "ICCP", 1, &it)) {
        o->icc = dup_bytes(it.chunk.bytes, it.chunk.size);
        o->icc_len = (int32_t)it.chunk.size;
        WebPDemuxReleaseChunkIterator(&it);
    }
    if (WebPDemuxGetChunk(demux, "EXIF", 1, &it)) {
        o->exif = dup_bytes(it.chunk.bytes, it.chunk.size);
        o->exif_len = (int32_t)it.chunk.size;
        WebPDemuxReleaseChunkIterator(&it);
    }
    if (WebPDemuxGetChunk(demux, "XMP ", 1, &it)) {
        o->xmp = dup_bytes(it.chunk.bytes, it.chunk.size);
        o->xmp_len = (int32_t)it.chunk.size;
        WebPDemuxReleaseChunkIterator(&it);
    }
    o->frames = (int32_t)WebPDemuxGetI(demux, WEBP_FF_FRAME_COUNT);
    WebPDemuxDelete(demux);
}

int gt_webp_decode(const uint8_t* data, size_t len, gt_img* o) {
    init_img(o);
    WebPBitstreamFeatures f;
    if (WebPGetFeatures(data, len, &f) != VP8_STATUS_OK) { set_err(o, "webp: invalid file%s", NULL); return -1; }
    webp_chunks(data, len, o);
    int w = 0, h = 0;
    if (f.has_animation) {
        int frames = 0, loops = 0;
        void* a = gt_webp_anim_open(data, len, &w, &h, &frames, &loops);
        if (!a) { set_err(o, "webp: animation decode failed%s", NULL); gt_free(o); return -1; }
        o->pixels = (uint8_t*)malloc((size_t)w * h * 4);
        int ts = 0;
        if (!o->pixels || !gt_webp_anim_next(a, o->pixels, &ts)) { gt_webp_anim_close(a); gt_free(o); return -1; }
        gt_webp_anim_close(a);
        o->frames = frames;
        o->delay_ms = ts;
    } else {
        o->pixels = WebPDecodeRGBA(data, len, &w, &h);
        if (!o->pixels) { set_err(o, "webp: decode failed%s", NULL); gt_free(o); return -1; }
        /* WebPDecodeRGBA memory must be released with WebPFree: copy into malloc memory. */
        uint8_t* copy = (uint8_t*)malloc((size_t)w * h * 4);
        if (copy) memcpy(copy, o->pixels, (size_t)w * h * 4);
        WebPFree(o->pixels);
        o->pixels = copy;
    }
    o->width = w;
    o->height = h;
    o->format = GT_RGBA8;
    o->stride = w * 4;
    o->has_alpha = f.has_alpha;
    o->bit_depth = 8;
    return o->pixels ? 0 : -1;
}

typedef struct { WebPAnimDecoder* dec; int w, h; } gt_anim;

void* gt_webp_anim_open(const uint8_t* data, size_t len, int* w, int* h, int* frames, int* loops) {
    WebPAnimDecoderOptions opt;
    if (!WebPAnimDecoderOptionsInit(&opt)) return NULL;
    opt.color_mode = MODE_RGBA;
    opt.use_threads = 1;
    WebPData wd = {data, len};
    WebPAnimDecoder* dec = WebPAnimDecoderNew(&wd, &opt);
    if (!dec) return NULL;
    WebPAnimInfo info;
    if (!WebPAnimDecoderGetInfo(dec, &info)) { WebPAnimDecoderDelete(dec); return NULL; }
    gt_anim* a = (gt_anim*)calloc(1, sizeof(gt_anim));
    if (!a) { WebPAnimDecoderDelete(dec); return NULL; }
    a->dec = dec;
    a->w = (int)info.canvas_width;
    a->h = (int)info.canvas_height;
    *w = a->w;
    *h = a->h;
    *frames = (int)info.frame_count;
    *loops = (int)info.loop_count;
    return a;
}

int gt_webp_anim_next(void* p, uint8_t* rgba_out, int* timestamp_ms) {
    gt_anim* a = (gt_anim*)p;
    if (!a || !WebPAnimDecoderHasMoreFrames(a->dec)) return 0;
    uint8_t* buf = NULL;
    int ts = 0;
    if (!WebPAnimDecoderGetNext(a->dec, &buf, &ts)) return 0;
    memcpy(rgba_out, buf, (size_t)a->w * a->h * 4);
    *timestamp_ms = ts;
    return 1;
}

void gt_webp_anim_close(void* p) {
    gt_anim* a = (gt_anim*)p;
    if (!a) return;
    WebPAnimDecoderDelete(a->dec);
    free(a);
}

/* ------------------------------------------------------------------ JPEG */

int gt_jpeg_header(const uint8_t* data, size_t len, int* w, int* h, int* cmyk) {
    tjhandle t = tj3Init(TJINIT_DECOMPRESS);
    if (!t) return -1;
    int r = tj3DecompressHeader(t, data, len);
    if (r == 0) {
        *w = tj3Get(t, TJPARAM_JPEGWIDTH);
        *h = tj3Get(t, TJPARAM_JPEGHEIGHT);
        int cs = tj3Get(t, TJPARAM_COLORSPACE);
        *cmyk = cs == TJCS_CMYK || cs == TJCS_YCCK;
    }
    tj3Destroy(t);
    return r;
}

int gt_jpeg_decode(const uint8_t* data, size_t len, int scale_denom, gt_img* o) {
    init_img(o);
    tjhandle t = tj3Init(TJINIT_DECOMPRESS);
    if (!t) { set_err(o, "jpeg: init failed%s", NULL); return -1; }
    if (tj3DecompressHeader(t, data, len) != 0) {
        set_err(o, "jpeg: %s", tj3GetErrorStr(t));
        tj3Destroy(t);
        return -1;
    }
    int w = tj3Get(t, TJPARAM_JPEGWIDTH), h = tj3Get(t, TJPARAM_JPEGHEIGHT);
    int cs = tj3Get(t, TJPARAM_COLORSPACE);
    int cmyk = cs == TJCS_CMYK || cs == TJCS_YCCK;
    o->full_width = w;
    o->full_height = h;
    if (scale_denom > 1) {
        tjscalingfactor sf = {1, scale_denom};
        if (tj3SetScalingFactor(t, sf) == 0) {
            w = TJSCALED(w, sf);
            h = TJSCALED(h, sf);
        }
    }
    int pf = cmyk ? TJPF_CMYK : TJPF_RGBA;
    o->pixels = (uint8_t*)malloc((size_t)w * h * 4);
    if (!o->pixels) { tj3Destroy(t); return -1; }
    tj3Set(t, TJPARAM_STOPONWARNING, 0);
    if (tj3Decompress8(t, data, len, o->pixels, 0, pf) != 0 && tj3GetErrorCode(t) == TJERR_FATAL) {
        set_err(o, "jpeg: %s", tj3GetErrorStr(t));
        tj3Destroy(t);
        gt_free(o);
        return -1;
    }
    tj3Destroy(t);
    o->width = w;
    o->height = h;
    o->format = cmyk ? GT_CMYK8 : GT_RGBA8;
    o->stride = w * 4;
    o->bit_depth = 8;
    return 0;
}

/* ------------------------------------------------------------------ TIFF */

static void tiff_silence(const char* m, const char* f, va_list ap) { (void)m; (void)f; (void)ap; }

int gt_tiff_decode(const wchar_t* path, gt_img* o) {
    init_img(o);
    TIFFSetWarningHandler(tiff_silence);
    TIFFSetErrorHandler(tiff_silence);
    TIFF* t = TIFFOpenW(path, "r");
    if (!t) { set_err(o, "tiff: cannot open%s", NULL); return -1; }
    uint32_t w = 0, h = 0;
    uint16_t spp = 1, bps = 8, sf = SAMPLEFORMAT_UINT, photo = PHOTOMETRIC_RGB, planar = PLANARCONFIG_CONTIG, orient = ORIENTATION_TOPLEFT;
    TIFFGetField(t, TIFFTAG_IMAGEWIDTH, &w);
    TIFFGetField(t, TIFFTAG_IMAGELENGTH, &h);
    TIFFGetFieldDefaulted(t, TIFFTAG_SAMPLESPERPIXEL, &spp);
    TIFFGetFieldDefaulted(t, TIFFTAG_BITSPERSAMPLE, &bps);
    TIFFGetFieldDefaulted(t, TIFFTAG_SAMPLEFORMAT, &sf);
    TIFFGetFieldDefaulted(t, TIFFTAG_PHOTOMETRIC, &photo);
    TIFFGetFieldDefaulted(t, TIFFTAG_PLANARCONFIG, &planar);
    TIFFGetFieldDefaulted(t, TIFFTAG_ORIENTATION, &orient);
    uint32_t icc_len = 0;
    void* icc = NULL;
    if (TIFFGetField(t, TIFFTAG_ICCPROFILE, &icc_len, &icc) && icc_len) {
        o->icc = dup_bytes((const uint8_t*)icc, icc_len);
        o->icc_len = (int32_t)icc_len;
    }
    uint16_t extra_n = 0;
    uint16_t* extra = NULL;
    TIFFGetField(t, TIFFTAG_EXTRASAMPLES, &extra_n, &extra);
    o->frames = TIFFNumberOfDirectories(t);
    o->width = (int32_t)w;
    o->height = (int32_t)h;
    o->bit_depth = bps;
    int color_ch = (photo == PHOTOMETRIC_MINISBLACK || photo == PHOTOMETRIC_MINISWHITE) ? 1 : (photo == PHOTOMETRIC_SEPARATED ? 4 : 3);
    int has_alpha = extra_n > 0 && spp > color_ch;
    o->has_alpha = has_alpha;
    int deep = (bps == 16 || bps == 32) && (photo == PHOTOMETRIC_RGB || photo == PHOTOMETRIC_MINISBLACK) &&
               (sf == SAMPLEFORMAT_UINT || sf == SAMPLEFORMAT_IEEEFP);
    int cmyk8 = photo == PHOTOMETRIC_SEPARATED && bps == 8 && spp >= 4 && planar == PLANARCONFIG_CONTIG && !TIFFIsTiled(t);
    if (deep) {
        /* 16-bit integer, half or float: read scanlines or tiles into RGBA. */
        int bytes = bps / 8;
        int out_float = sf == SAMPLEFORMAT_IEEEFP;
        int out_bpc = out_float ? (bps == 16 ? 2 : 4) : 2;
        o->format = out_float ? (bps == 16 ? GT_RGBAF16 : GT_RGBAF32) : GT_RGBA16;
        o->stride = (int32_t)w * 4 * out_bpc;
        o->pixels = (uint8_t*)calloc((size_t)o->stride, h);
        if (!o->pixels) { TIFFClose(t); return -1; }
        tmsize_t row_bytes = TIFFScanlineSize(t);
        uint8_t* src = NULL;
        int ok = 1;
        if (TIFFIsTiled(t)) {
            uint32_t tw = 0, th = 0;
            TIFFGetField(t, TIFFTAG_TILEWIDTH, &tw);
            TIFFGetField(t, TIFFTAG_TILELENGTH, &th);
            tmsize_t tsize = TIFFTileSize(t);
            src = (uint8_t*)malloc(tsize);
            int planes = planar == PLANARCONFIG_SEPARATE ? spp : 1;
            for (int pl = 0; pl < planes && ok; pl++)
                for (uint32_t ty = 0; ty < h && ok; ty += th)
                    for (uint32_t tx = 0; tx < w && ok; tx += tw) {
                        if (TIFFReadTile(t, src, tx, ty, 0, (uint16_t)pl) < 0) { ok = 0; break; }
                        for (uint32_t yy = 0; yy < th && ty + yy < h; yy++)
                            for (uint32_t xx = 0; xx < tw && tx + xx < w; xx++) {
                                uint8_t* d = o->pixels + (size_t)(ty + yy) * o->stride + (size_t)(tx + xx) * 4 * out_bpc;
                                int nsamp = planar == PLANARCONFIG_SEPARATE ? 1 : spp;
                                for (int c = 0; c < nsamp; c++) {
                                    int dc = planar == PLANARCONFIG_SEPARATE ? pl : c;
                                    if (dc > 3) continue;
                                    const uint8_t* s = src + ((size_t)yy * tw + xx) * nsamp * bytes + c * bytes;
                                    memcpy(d + dc * out_bpc, s, out_bpc);
                                }
                            }
                    }
        } else {
            src = (uint8_t*)malloc(row_bytes);
            int planes = planar == PLANARCONFIG_SEPARATE ? spp : 1;
            for (int pl = 0; pl < planes && ok; pl++)
                for (uint32_t y = 0; y < h; y++) {
                    if (TIFFReadScanline(t, src, y, (uint16_t)pl) < 0) { ok = 0; break; }
                    uint8_t* d = o->pixels + (size_t)y * o->stride;
                    int nsamp = planar == PLANARCONFIG_SEPARATE ? 1 : spp;
                    for (uint32_t x = 0; x < w; x++)
                        for (int c = 0; c < nsamp; c++) {
                            int dc = planar == PLANARCONFIG_SEPARATE ? pl : c;
                            if (dc > 3) continue;
                            memcpy(d + (size_t)x * 4 * out_bpc + dc * out_bpc, src + ((size_t)x * nsamp + c) * bytes, out_bpc);
                        }
                }
        }
        free(src);
        /* Gray → RGB, and opaque alpha when missing. */
        for (uint32_t y = 0; y < h; y++)
            for (uint32_t x = 0; x < w; x++) {
                uint8_t* p = o->pixels + (size_t)y * o->stride + (size_t)x * 4 * out_bpc;
                if (color_ch == 1) {
                    if (has_alpha) memcpy(p + 3 * out_bpc, p + out_bpc, out_bpc);
                    memcpy(p + out_bpc, p, out_bpc);
                    memcpy(p + 2 * out_bpc, p, out_bpc);
                }
                if (!has_alpha) {
                    if (o->format == GT_RGBA16) { uint16_t one = 65535; memcpy(p + 3 * out_bpc, &one, 2); }
                    else if (o->format == GT_RGBAF16) { uint16_t one = 0x3C00; memcpy(p + 3 * out_bpc, &one, 2); }
                    else { float one = 1.0f; memcpy(p + 3 * out_bpc, &one, 4); }
                }
            }
        o->orientation = orient >= 1 && orient <= 8 ? orient : 1;
        if (!ok) { set_err(o, "tiff: read error%s", NULL); TIFFClose(t); gt_free(o); return -1; }
    } else if (cmyk8) {
        o->format = GT_CMYK8;
        o->stride = (int32_t)w * 4;
        o->pixels = (uint8_t*)malloc((size_t)o->stride * h);
        uint8_t* src = (uint8_t*)malloc(TIFFScanlineSize(t));
        if (!o->pixels || !src) { free(src); TIFFClose(t); gt_free(o); return -1; }
        for (uint32_t y = 0; y < h; y++) {
            if (TIFFReadScanline(t, src, y, 0) < 0) break;
            for (uint32_t x = 0; x < w; x++) memcpy(o->pixels + (size_t)y * o->stride + x * 4, src + (size_t)x * spp, 4);
        }
        free(src);
        o->orientation = orient >= 1 && orient <= 8 ? orient : 1;
    } else {
        /* Everything else through libtiff's RGBA path (handles orientation). */
        o->format = GT_RGBA8;
        o->stride = (int32_t)w * 4;
        o->pixels = (uint8_t*)malloc((size_t)o->stride * h);
        if (!o->pixels || !TIFFReadRGBAImageOriented(t, w, h, (uint32_t*)o->pixels, ORIENTATION_TOPLEFT, 0)) {
            set_err(o, "tiff: unsupported layout%s", NULL);
            TIFFClose(t);
            gt_free(o);
            return -1;
        }
        o->bit_depth = 8;
        if (photo == PHOTOMETRIC_SEPARATED) { free(o->icc); o->icc = NULL; o->icc_len = 0; } /* already converted naively */
    }
    TIFFClose(t);
    return 0;
}

/* ------------------------------------------------------------------ JPEG 2000 */

typedef struct { const uint8_t* data; size_t len, pos; } memsrc;

static OPJ_SIZE_T ms_read(void* buf, OPJ_SIZE_T n, void* user) {
    memsrc* m = (memsrc*)user;
    if (m->pos >= m->len) return (OPJ_SIZE_T)-1;
    size_t k = m->len - m->pos < n ? m->len - m->pos : n;
    memcpy(buf, m->data + m->pos, k);
    m->pos += k;
    return k;
}
static OPJ_OFF_T ms_skip(OPJ_OFF_T n, void* user) {
    memsrc* m = (memsrc*)user;
    if (n < 0 && (size_t)(-n) > m->pos) n = -(OPJ_OFF_T)m->pos;
    if (n > 0 && m->pos + (size_t)n > m->len) n = (OPJ_OFF_T)(m->len - m->pos);
    m->pos += n;
    return n;
}
static OPJ_BOOL ms_seek(OPJ_OFF_T n, void* user) {
    memsrc* m = (memsrc*)user;
    if (n < 0 || (size_t)n > m->len) return OPJ_FALSE;
    m->pos = (size_t)n;
    return OPJ_TRUE;
}
static void opj_quiet(const char* msg, void* u) { (void)msg; (void)u; }

int gt_j2k_decode(const uint8_t* data, size_t len, int max_w, int max_h, gt_img* o) {
    init_img(o);
    OPJ_CODEC_FORMAT fmt = (len > 4 && data[0] == 0xFF && data[1] == 0x4F) ? OPJ_CODEC_J2K : OPJ_CODEC_JP2;
    memsrc ms = {data, len, 0};
    opj_stream_t* st = opj_stream_create(1 << 16, OPJ_TRUE);
    opj_stream_set_user_data(st, &ms, NULL);
    opj_stream_set_user_data_length(st, len);
    opj_stream_set_read_function(st, ms_read);
    opj_stream_set_skip_function(st, ms_skip);
    opj_stream_set_seek_function(st, ms_seek);
    opj_codec_t* c = opj_create_decompress(fmt);
    opj_set_info_handler(c, opj_quiet, NULL);
    opj_set_warning_handler(c, opj_quiet, NULL);
    opj_set_error_handler(c, opj_quiet, NULL);
    opj_dparameters_t p;
    opj_set_default_decoder_parameters(&p);
    opj_image_t* img = NULL;
    int ok = opj_setup_decoder(c, &p) && opj_read_header(st, c, &img);
    if (ok) {
        /* Decode at the smallest resolution level that still covers the screen. */
        opj_codestream_info_v2_t* cs = opj_get_cstr_info(c);
        int levels = cs ? (int)cs->m_default_tile_info.tccp_info[0].numresolutions : 1;
        if (cs) opj_destroy_cstr_info(&cs);
        int reduce = 0;
        int iw = (int)(img->x1 - img->x0), ih = (int)(img->y1 - img->y0);
        o->full_width = iw;
        o->full_height = ih;
        while (max_w > 0 && reduce + 1 < levels && (iw >> (reduce + 1)) >= max_w && (ih >> (reduce + 1)) >= max_h) reduce++;
        if (reduce > 0) opj_set_decoded_resolution_factor(c, (OPJ_UINT32)reduce);
        opj_codec_set_threads(c, cpu_threads());
        ok = opj_decode(c, st, img) && opj_end_decompress(c, st);
    }
    opj_stream_destroy(st);
    opj_destroy_codec(c);
    if (!ok || !img || img->numcomps < 1) {
        set_err(o, "jpeg2000: decode failed%s", NULL);
        if (img) opj_image_destroy(img);
        return -1;
    }
    opj_image_comp_t* c0 = &img->comps[0];
    int w = (int)c0->w, h = (int)c0->h, nc = (int)img->numcomps;
    int prec = (int)c0->prec;
    int deep = prec > 8;
    o->width = w;
    o->height = h;
    o->bit_depth = prec;
    o->format = deep ? GT_RGBA16 : GT_RGBA8;
    o->stride = w * (deep ? 8 : 4);
    o->pixels = (uint8_t*)malloc((size_t)o->stride * h);
    if (img->icc_profile_buf && img->icc_profile_len) {
        o->icc = dup_bytes(img->icc_profile_buf, img->icc_profile_len);
        o->icc_len = (int32_t)img->icc_profile_len;
    }
    int ycc = img->color_space == OPJ_CLRSPC_SYCC || img->color_space == OPJ_CLRSPC_EYCC;
    o->has_alpha = nc == 2 || nc >= 4;
    for (int y = 0; y < h && o->pixels; y++)
        for (int x = 0; x < w; x++) {
            double v[4] = {0, 0, 0, 1};
            for (int k = 0; k < nc && k < 4; k++) {
                opj_image_comp_t* ck = &img->comps[k];
                int sx = x * (int)ck->w / w, sy = y * (int)ck->h / h;
                int raw = ck->data[(size_t)sy * ck->w + sx];
                if (ck->sgnd) raw += 1 << (ck->prec - 1);
                v[k] = (double)raw / (double)((1 << ck->prec) - 1);
            }
            double r, g, b, a = 1.0;
            if (nc <= 2) { r = g = b = v[0]; if (nc == 2) a = v[1]; }
            else {
                r = v[0]; g = v[1]; b = v[2];
                if (ycc) {
                    double yy = v[0], cb = v[1] - 0.5, cr = v[2] - 0.5;
                    r = yy + 1.402 * cr; g = yy - 0.344136 * cb - 0.714136 * cr; b = yy + 1.772 * cb;
                }
                if (nc >= 4) a = v[3];
            }
            double px[4] = {r, g, b, a};
            for (int k = 0; k < 4; k++) {
                double q = px[k] < 0 ? 0 : (px[k] > 1 ? 1 : px[k]);
                if (deep) ((uint16_t*)(o->pixels + (size_t)y * o->stride))[x * 4 + k] = (uint16_t)(q * 65535.0 + 0.5);
                else (o->pixels + (size_t)y * o->stride)[x * 4 + k] = (uint8_t)(q * 255.0 + 0.5);
            }
        }
    opj_image_destroy(img);
    return o->pixels ? 0 : -1;
}

/* ------------------------------------------------------------------ RAW */

static int flip_to_exif(int flip) {
    switch (flip) {
    case 3: return 3;
    case 5: return 8;
    case 6: return 6;
    default: return 1;
    }
}

int gt_raw_thumb(const uint8_t* data, size_t len, gt_img* bm, uint8_t** jpeg, size_t* jpeg_len, int* raw_w, int* raw_h, int* flip) {
    init_img(bm);
    *jpeg = NULL;
    *jpeg_len = 0;
    libraw_data_t* r = libraw_init(0);
    if (!r) return -1;
    if (libraw_open_buffer(r, data, len) != LIBRAW_SUCCESS) { libraw_close(r); return -1; }
    *raw_w = r->sizes.width;
    *raw_h = r->sizes.height;
    *flip = flip_to_exif(r->sizes.flip);
    int kind = 0;
    if (libraw_unpack_thumb(r) == LIBRAW_SUCCESS) {
        int err = 0;
        libraw_processed_image_t* t = libraw_dcraw_make_mem_thumb(r, &err);
        if (t) {
            if (t->type == LIBRAW_IMAGE_JPEG) {
                *jpeg = dup_bytes(t->data, t->data_size);
                *jpeg_len = t->data_size;
                kind = *jpeg ? 1 : 0;
            } else if (t->type == LIBRAW_IMAGE_BITMAP && t->bits == 8 && (t->colors == 3 || t->colors == 1)) {
                bm->pixels = expand_rgba8(t->data, t->width, t->height, (size_t)t->width * t->colors, t->colors);
                bm->width = t->width;
                bm->height = t->height;
                bm->stride = t->width * 4;
                bm->format = GT_RGBA8;
                bm->bit_depth = 8;
                kind = bm->pixels ? 2 : 0;
            }
            libraw_dcraw_clear_mem(t);
        }
    }
    libraw_close(r);
    return kind;
}

int gt_raw_process(const uint8_t* data, size_t len, int half_size, gt_img* o) {
    init_img(o);
    libraw_data_t* r = libraw_init(0);
    if (!r) return -1;
    r->params.half_size = half_size;
    r->params.use_camera_wb = 1;
    r->params.output_bps = 16;
    r->params.output_color = 1; /* sRGB primaries, BT.709 curve */
    int rc = libraw_open_buffer(r, data, len);
    if (rc == LIBRAW_SUCCESS) rc = libraw_unpack(r);
    if (rc == LIBRAW_SUCCESS) rc = libraw_dcraw_process(r);
    if (rc != LIBRAW_SUCCESS) {
        set_err(o, "raw: %s", libraw_strerror(rc));
        libraw_close(r);
        return -1;
    }
    int err = 0;
    libraw_processed_image_t* img = libraw_dcraw_make_mem_image(r, &err);
    if (!img || img->bits != 16 || img->colors < 3) {
        set_err(o, "raw: no processed image%s", NULL);
        if (img) libraw_dcraw_clear_mem(img);
        libraw_close(r);
        return -1;
    }
    int w = img->width, h = img->height;
    uint16_t* out = (uint16_t*)malloc((size_t)w * h * 8);
    if (out) {
        const uint16_t* s = (const uint16_t*)img->data;
        for (size_t i = 0; i < (size_t)w * h; i++) {
            out[i * 4] = s[i * img->colors];
            out[i * 4 + 1] = s[i * img->colors + 1];
            out[i * 4 + 2] = s[i * img->colors + 2];
            out[i * 4 + 3] = 65535;
        }
    }
    o->pixels = (uint8_t*)out;
    o->width = w;
    o->height = h;
    o->stride = w * 8;
    o->format = GT_RGBA16;
    o->bit_depth = 16;
    o->cicp_primaries = 1;
    o->cicp_transfer = 1; /* LibRaw default output curve is BT.709 */
    o->full_width = r->sizes.width;
    o->full_height = r->sizes.height;
    libraw_dcraw_clear_mem(img);
    libraw_close(r);
    return out ? 0 : -1;
}
