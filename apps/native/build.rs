//! Embeds `app.manifest` with the MSVC linker, so no resource compiler or
//! extra crate is needed.
//!
//! The manifest goes into the test executables as well: without it they load
//! version 5 of the common controls, which lacks functions the crate imports.

fn main() {
    println!("cargo:rerun-if-changed=app.manifest");
    println!("cargo:rerun-if-changed=build.rs");
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_env != "msvc" {
        return;
    }
    let dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let manifest = std::path::Path::new(&dir).join("app.manifest");
    println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());
}
