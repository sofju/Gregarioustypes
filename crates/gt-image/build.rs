//! Compiles the native shim and links the static vcpkg libraries (triplet
//! x64-windows-static, /MT, matching `+crt-static`).

use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = manifest.join("..").join("..");
    let vcpkg = std::env::var("GT_VCPKG_INSTALLED").map(PathBuf::from).unwrap_or_else(|_| root.join("vcpkg_installed").join("x64-windows-static"));
    let include = vcpkg.join("include");
    let lib = vcpkg.join("lib");
    assert!(include.is_dir(), "vcpkg libraries not found at {} — run vcpkg install (see README)", vcpkg.display());

    let defines = [
        ("LIBRAW_NODLL", None),
        ("LIBHEIF_STATIC_BUILD", None),
        ("JXL_STATIC_DEFINE", None),
        ("JXL_THREADS_STATIC_DEFINE", None),
        ("JXL_CMS_STATIC_DEFINE", None),
        ("OPJ_STATIC", None),
        ("WIN32_LEAN_AND_MEAN", None),
        ("NOMINMAX", None),
    ];

    let mut c = cc::Build::new();
    c.file("native/gt_codecs.c").include(&include).include(include.join("openjpeg-2.5")).include(include.join("libraw")).warnings(false).opt_level(2);
    for (k, v) in defines {
        c.define(k, v);
    }
    c.compile("gt_codecs");

    let mut cpp = cc::Build::new();
    cpp.cpp(true).file("native/gt_exr.cpp").include(&include).include(include.join("OpenEXR")).include(include.join("Imath")).flag_if_supported("/std:c++17").flag_if_supported("/EHsc").warnings(false).opt_level(2);
    cpp.compile("gt_exr");

    println!("cargo:rustc-link-search=native={}", lib.display());
    for l in [
        "avif", "yuv", "dav1d", "heif", "libde265", "openjp2", "jxl", "jxl_cms", "jxl_threads", "hwy", "brotlidec", "brotlienc", "brotlicommon", "raw_r", "lcms2",
        "turbojpeg", "spng_static", "jpeg", "tiff", "libwebp", "libwebpdemux", "libsharpyuv", "zs", "zstd", "lzma", "deflatestatic", "OpenEXR-3_5", "OpenEXRUtil-3_5",
        "OpenEXRCore-3_5", "Iex-3_5", "IlmThread-3_5", "Imath-3_2", "openjph.0.32",
    ] {
        println!("cargo:rustc-link-lib=static={l}");
    }
    for l in ["ws2_32", "advapi32", "user32"] {
        println!("cargo:rustc-link-lib=dylib={l}");
    }
    println!("cargo:rerun-if-changed=native/gt_codecs.c");
    println!("cargo:rerun-if-changed=native/gt_codecs.h");
    println!("cargo:rerun-if-changed=native/gt_exr.cpp");
}
