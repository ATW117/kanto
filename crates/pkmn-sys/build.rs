//! Builds libpkmn (pkmn/engine) from the `third_party/pkmn-engine` submodule with Zig and links
//! it statically. Configuration: Gen 1 in Pokémon Showdown compatibility mode with protocol
//! logging enabled (`-Dshowdown -Dlog`), position independent so it can be linked into the Python
//! extension module.
//!
//! Environment overrides:
//!   ZIG            path to the zig 0.16 binary (default: `zig` on PATH)
//!   PKMN_OPTIMIZE  zig optimize mode (default ReleaseFast; ReleaseSafe enables engine asserts)
//!   PKMN_CPU       zig -Dcpu value (default: native)

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn find_zig() -> PathBuf {
    if let Ok(z) = env::var("ZIG") {
        return PathBuf::from(z);
    }
    let on_path = Command::new("zig").arg("version").output();
    if matches!(on_path, Ok(ref o) if o.status.success()) {
        return PathBuf::from("zig");
    }
    panic!("could not find zig 0.16; set ZIG=/path/to/zig");
}

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let engine = manifest
        .join("../../third_party/pkmn-engine")
        .canonicalize()
        .unwrap_or_else(|_| panic!("third_party/pkmn-engine is missing; run `git submodule update --init`"));
    if !engine.join("build.zig").exists() {
        panic!("third_party/pkmn-engine is empty; run `git submodule update --init`");
    }
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let prefix = out.join("pkmn");
    let optimize = env::var("PKMN_OPTIMIZE").unwrap_or_else(|_| "ReleaseFast".into());
    let cpu = env::var("PKMN_CPU").unwrap_or_else(|_| "native".into());

    let zig = find_zig();
    let status = Command::new(&zig)
        .current_dir(&engine)
        .args(["build", "-Dshowdown=true", "-Dlog=true", "-Dpic=true"])
        .arg(format!("-Doptimize={optimize}"))
        .arg(format!("-Dcpu={cpu}"))
        .arg("--prefix")
        .arg(&prefix)
        // Keep zig's local cache out of the submodule's working tree.
        .arg("--cache-dir")
        .arg(out.join("zig-cache"))
        .status()
        .unwrap_or_else(|e| panic!("failed to run {}: {e}", zig.display()));
    assert!(status.success(), "zig build of libpkmn failed");

    println!("cargo:rustc-link-search=native={}", prefix.join("lib").display());
    println!("cargo:rustc-link-lib=static=pkmn-showdown");
    println!("cargo:include={}", prefix.join("include").display());
    // Exposed as `pkmn_sys::LIB_DIR` / `INCLUDE_DIR` so tests can build the C cross-check driver.
    println!("cargo:rustc-env=PKMN_LIB_DIR={}", prefix.join("lib").display());
    println!("cargo:rustc-env=PKMN_INCLUDE_DIR={}", prefix.join("include").display());
    println!("cargo:rerun-if-changed={}", engine.join("src/lib").display());
    println!("cargo:rerun-if-changed={}", engine.join("build.zig").display());
    println!("cargo:rerun-if-env-changed=ZIG");
    println!("cargo:rerun-if-env-changed=PKMN_OPTIMIZE");
    println!("cargo:rerun-if-env-changed=PKMN_CPU");
}
