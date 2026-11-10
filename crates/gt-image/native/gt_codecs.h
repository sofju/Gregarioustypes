/* Minimal, stable C ABI over the image libraries (built by vcpkg). Rust only ever
   sees gt_img; library struct layouts stay on the C side. */
#pragma once
#include <stddef.h>
#include <stdint.h>
#include <wchar.h>

#ifdef __cplusplus
extern "C" {
#endif

enum { GT_RGBA8 = 0, GT_RGBA16 = 1, GT_RGBAF16 = 2, GT_RGBAF32 = 3, GT_CMYK8 = 4 };
enum { GT_GM_NONE = 0, GT_GM_ISO = 1, GT_GM_APPLE = 2 };

typedef struct gt_img {
    int32_t width, height, format, stride;
    uint8_t* pixels;
    uint8_t* icc;
    int32_t icc_len;
    /* CICP / nclx; -1 when absent */
    int32_t cicp_primaries, cicp_transfer, cicp_matrix, cicp_full;
    /* custom chromaticities: rx ry gx gy bx by wx wy */
    int32_t has_prim;
    float prim[8];
    float gamma; /* explicit gamma (JXL), 0 if none */
    int32_t has_alpha, bit_depth, orientation;
    float max_cll, max_fall, mastering_max;
    uint8_t* exif;
    int32_t exif_len;
    uint8_t* xmp;
    int32_t xmp_len;
    struct gt_img* gainmap;
    int32_t gm_kind;
    float gm_min[3], gm_max[3], gm_gamma[3], gm_base_offset[3], gm_alt_offset[3];
    float gm_base_headroom, gm_alt_headroom; /* log2 */
    int32_t gm_use_base_cs;
    int32_t frames, delay_ms;
    int32_t full_width, full_height;
    float intensity_target;
    char error[256];
} gt_img;

void gt_free(gt_img* img);
void gt_free_buf(void* p);

int gt_avif_decode(const uint8_t* data, size_t len, gt_img* out);
int gt_heif_decode(const uint8_t* data, size_t len, gt_img* out);
int gt_jxl_decode(const uint8_t* data, size_t len, gt_img* out);
int gt_webp_decode(const uint8_t* data, size_t len, gt_img* out);
void* gt_webp_anim_open(const uint8_t* data, size_t len, int* w, int* h, int* frames, int* loops);
int gt_webp_anim_next(void* h, uint8_t* rgba_out, int* timestamp_ms);
void gt_webp_anim_close(void* h);
int gt_jpeg_header(const uint8_t* data, size_t len, int* w, int* h, int* cmyk);
int gt_jpeg_decode(const uint8_t* data, size_t len, int scale_denom, gt_img* out);
int gt_tiff_decode(const wchar_t* path, gt_img* out);
int gt_j2k_decode(const uint8_t* data, size_t len, int max_w, int max_h, gt_img* out);
int gt_raw_thumb(const uint8_t* data, size_t len, gt_img* bitmap, uint8_t** jpeg, size_t* jpeg_len, int* raw_w, int* raw_h, int* flip);
int gt_raw_process(const uint8_t* data, size_t len, int half_size, gt_img* out);
int gt_exr_decode(const char* utf8_path, gt_img* out);

#ifdef __cplusplus
}
#endif
