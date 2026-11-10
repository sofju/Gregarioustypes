//! Compiles the Alembic shim and links the static vcpkg Alembic and Imath
//! (triplet x64-windows-static, /MT, matching `+crt-static`).

use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = manifest.join("..").join("..");
    let vcpkg = std::env::var("GT_VCPKG_INSTALLED").map(PathBuf::from).unwrap_or_else(|_| root.join("vcpkg_installed").join("x64-windows-static"));
    let include = vcpkg.join("include");
    assert!(include.join("Alembic").is_dir(), "Alembic not found at {} — run vcpkg install (see README)", vcpkg.display());
    cc::Build::new()
        .cpp(true)
        .file("native/gt_abc.cpp")
        .include(&include)
        .include(include.join("Imath"))
        .define("NOMINMAX", None)
        .flag_if_supported("/std:c++17")
        .flag_if_supported("/EHsc")
        .warnings(false)
        .opt_level(2)
        .compile("gt_abc");
    println!("cargo:rustc-link-search=native={}", vcpkg.join("lib").display());
    for l in ["Alembic", "Imath-3_2"] {
        println!("cargo:rustc-link-lib=static={l}");
    }
    println!("cargo:rerun-if-changed=native/gt_abc.cpp");
}
