//! Build script for `webai-bridge-cxx`.
//!
//! With the default feature set (no `legacy_cpp`) this crate is pure Rust:
//! the build script does **nothing** and never invokes `pkg-config` or `cc`,
//! so the default build has zero system dependencies (ARCHITECTURE.md §4.7/§10).
//!
//! With `--features legacy_cpp` the build script:
//!   1. probes the cog / WPE / WPEBackend-fdo / cairo / glib libraries via
//!      `pkg-config`, failing with a **readable** error naming the missing
//!      package and install hint (never a link-time symbol soup);
//!   2. compiles the thin C++ wrapper (`cpp/wrapper.cc`) with `cxx-build`,
//!      merging it with the cxx-generated shim so the bridge symbols resolve.

#[cfg(feature = "legacy_cpp")]
use std::path::Path;

fn main() {
    // Default build: no C++, no pkg-config, no system WebKit.
    if std::env::var("CARGO_FEATURE_LEGACY_CPP").is_err() {
        println!("cargo:rerun-if-changed=build.rs");
    } else {
        // The rest of this file only compiles when `legacy_cpp` is enabled, so
        // the optional build-dependencies (cxx-build / cc / pkg-config) are only
        // referenced in that configuration.
        #[cfg(feature = "legacy_cpp")]
        build_legacy();
    }
}

#[cfg(feature = "legacy_cpp")]
fn build_legacy() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let wrapper_src = manifest_dir.join("cpp").join("wrapper.cc");
    let wrapper_include = manifest_dir.join("cpp");

    // Probe the required system libraries. Each probe fails with a readable
    // error naming the missing pkg and how to install it.
    let cog = probe("cogcore", "cog (>= 0.19)", "apt install libcog-dev");
    let wpe = probe(
        "wpe-webkit-2.0",
        "WPE WebKit (>= 2.50)",
        "apt install libwpewebkit-2.0-dev",
    );
    let wpe_fdo = probe(
        "wpebackend-fdo-1.0",
        "WPEBackend-fdo (>= 1.16)",
        "apt install libwpebackend-fdo-dev",
    );
    let cairo = probe("cairo", "cairo", "apt install libcairo2-dev");
    let glib = probe("glib-2.0", "GLib", "apt install libglib2.0-dev");

    // Build the cxx bridge. cxx-build compiles the generated shim; we then
    // merge the wrapper source above the shim so the function bodies the shim
    // references are visible at link time.
    let mut build = cxx_build::bridge("src/lib.rs");

    if let Some(impl_path) = cxx_impl_path() {
        let wrapper_body =
            std::fs::read_to_string(&wrapper_src).expect("failed to read cpp/wrapper.cc");
        let existing = std::fs::read_to_string(&impl_path).unwrap_or_default();
        let merged = merge_sources(&existing, &wrapper_body);
        std::fs::write(&impl_path, merged).expect("failed to write merged cxx impl");
    }

    build
        .include(wrapper_include.to_str().unwrap())
        .includes(&cog.include_paths)
        .includes(&wpe.include_paths)
        .includes(&wpe_fdo.include_paths)
        .includes(&cairo.include_paths)
        .includes(&glib.include_paths)
        .flag_if_supported("-std=c++20");

    build.compile("webai_bridge_cxx");

    println!("cargo:rerun-if-changed=src/lib.rs");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", wrapper_src.display());
}

/// Probe a pkg-config package, failing with a readable error.
#[cfg(feature = "legacy_cpp")]
fn probe(pkg: &str, human: &str, install_hint: &str) -> pkg_config::Library {
    pkg_config::Config::new()
        .cargo_metadata(true)
        .probe(pkg)
        .unwrap_or_else(|_| {
            panic!(
                "webai-bridge-cxx: missing system dependency `{pkg}` ({human}).\n\
                 Install it, e.g. `{install_hint}`, then rebuild with \
                 `--features legacy_cpp`."
            )
        })
}

/// Path of the cxx-generated `*.cc` implementation file.
#[cfg(feature = "legacy_cpp")]
fn cxx_impl_path() -> Option<std::path::PathBuf> {
    let out_dir = std::env::var_os("OUT_DIR")?;
    let path = Path::new(&out_dir)
        .join("cxxbridge")
        .join("sources")
        .join("webai-bridge-cxx")
        .join("src")
        .join("lib.rs.cc");
    if path.exists() {
        Some(path)
    } else {
        None
    }
}

/// Place the wrapper source above the cxx-generated shim so the function
/// bodies the shim references are visible at link time.
#[cfg(feature = "legacy_cpp")]
fn merge_sources(cxx_shim: &str, wrapper: &str) -> String {
    let mut out = String::with_capacity(cxx_shim.len() + wrapper.len());
    out.push_str("// === webai-bridge-cxx wrapper (prepended) ===\n\n");
    out.push_str(wrapper);
    out.push_str("\n\n// === cxx-generated shim (appended) ===\n\n");
    out.push_str(cxx_shim);
    out
}
