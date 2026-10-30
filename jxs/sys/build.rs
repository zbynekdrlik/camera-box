//! Issue 1387: build the vendored SVT-JPEG-XS (`vendor/svt-jpeg-xs`, pinned commit in
//! `vendor/svt-jpeg-xs/CAMERA-BOX-VENDOR.md`) and the camera-box C shim as static libraries.
//!
//! Why vendored and built here (not fetched, not a system package): the appliance binary must
//! build the same bytes offline and on every CI run. Ubuntu 22.04/24.04 ship no SVT-JPEG-XS
//! package, a download in build.rs depends on the network and on GitHub keeping the archive, and
//! the vendored tree is ~1.5 MB (the library sources + cpuinfo, no apps/tests/docs).
//!
//! Needs `cmake` (>= 3.10), a C/C++ compiler and `nasm` (>= 2.14; `ASM_NASM=<path>` picks one
//! that is not on PATH). The C library is always built in Release: a debug `cargo test` must not
//! run a -O0 codec.

use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let svt = manifest.join("../../vendor/svt-jpeg-xs");
    let svt = svt
        .canonicalize()
        .unwrap_or_else(|e| panic!("vendored SVT-JPEG-XS missing at {}: {e}", svt.display()));
    let csrc = manifest.join("csrc");

    println!("cargo:rerun-if-changed={}", csrc.display());
    println!(
        "cargo:rerun-if-changed={}",
        svt.join("CMakeLists.txt").display()
    );
    println!("cargo:rerun-if-changed={}", svt.join("Source").display());
    println!("cargo:rerun-if-env-changed=ASM_NASM");

    let dst = cmake::Config::new(&csrc)
        .profile("Release")
        .define("SVT_JXS_SOURCE_DIR", &svt)
        .build_target("all")
        .build();

    let lib = dst.join("build").join("lib");
    println!("cargo:rustc-link-search=native={}", lib.display());
    // The shim first: it references the library, the archive order matters for the linker.
    println!("cargo:rustc-link-lib=static=cbjxs_shim");
    println!("cargo:rustc-link-lib=static=SvtJpegxs");
    println!("cargo:rustc-link-lib=dylib=pthread");
    println!("cargo:rustc-link-lib=dylib=m");
}
