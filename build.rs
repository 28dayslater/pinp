// SPDX-License-Identifier: MIT
//
// Builds the pinp runtime — the mimalloc allocator plus our thin `pinp_*` shim
// (runtime/) — and links it into the pinp binary.
//
// The allocator runs as native host code, not inside the JIT: mimalloc keeps its
// per-thread heap in thread-local storage, and TLS accessed from JIT-compiled
// code emits relocations ORC cannot resolve. So JIT-compiled pinp code instead
// *calls* the runtime, with the `pinp_*` entry points resolved against the host
// process by the JIT's dynamic-library search generator (see src/codegen/jit.rs).
// Only the `pinp_*` surface is exported into the dynamic symbol table, so
// mimalloc's own symbols stay private to the binary.
//
// This runs only for the `llvm` feature: a front-end-only build
// (`--no-default-features`) has no JIT and needs no allocator or C toolchain.
//
// Caching: compiling mimalloc is the slow step, so its object is kept in OUT_DIR
// and recompiled only when missing or older than its source. Cargo already gates
// the whole script behind the `rerun-if-changed` inputs below, so an untouched
// tree never re-runs it at all.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

/// The runtime entry points made visible to the JIT; everything else (mimalloc)
/// stays out of the dynamic symbol table.
const EXPORTED_SYMBOLS: [&str; 3] = ["pinp_alloc", "pinp_free", "pinp_memory_info"];

fn main() {
    // The runtime is only needed by the LLVM backend.
    if std::env::var_os("CARGO_FEATURE_LLVM").is_none() {
        return;
    }

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    let mimalloc = PathBuf::from("third_party/mimalloc");
    let mimalloc_src = mimalloc.join("src/static.c");
    let mimalloc_include = mimalloc.join("include");
    let shim_src = Path::new("runtime/shim.c");
    let shim_header = Path::new("runtime/pinp_runtime.h");

    // Re-run only when the inputs change.
    for input in [mimalloc_src.as_path(), shim_src, shim_header] {
        println!("cargo:rerun-if-changed={}", input.display());
    }
    println!("cargo:rerun-if-changed=build.rs");

    if !mimalloc_src.exists() {
        panic!(
            "{} is missing — initialize the mimalloc submodule: \
             `git submodule update --init third_party/mimalloc`.",
            mimalloc_src.display()
        );
    }

    let mimalloc_object = out_dir.join("mimalloc.o");
    let shim_object = out_dir.join("shim.o");
    let archive = out_dir.join("libpinp_runtime.a");

    // mimalloc's amalgamation to a native object (cached — the slow step).
    if is_stale(&mimalloc_object, &mimalloc_src) {
        compile(&mimalloc_src, &[&mimalloc_include], &mimalloc_object);
    }
    // The shim, calling into mimalloc.
    compile(
        shim_src,
        &[&mimalloc_include, Path::new("runtime")],
        &shim_object,
    );

    // Archive both into the static library Cargo links below.
    let _ = std::fs::remove_file(&archive);
    run(
        "ar",
        Command::new("ar")
            .arg("crs")
            .arg(&archive)
            .arg(&mimalloc_object)
            .arg(&shim_object),
    );

    // Link the whole archive in (the binary never references the runtime directly —
    // the JIT resolves it at run time — so without `whole-archive` the linker would
    // drop it), and publish only the `pinp_*` names so the JIT's `dlsym`-based
    // search generator can find them.
    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static:+whole-archive=pinp_runtime");
    for symbol in EXPORTED_SYMBOLS {
        println!("cargo:rustc-link-arg=-Wl,--export-dynamic-symbol={symbol}");
    }
}

/// Compiles a C source to an optimized, position-independent native object.
fn compile(source: &Path, includes: &[&Path], output: &Path) {
    let mut command = Command::new("clang");
    command.args(["-O2", "-fPIC", "-c"]).arg(source);
    for include in includes {
        command.arg("-I").arg(include);
    }
    command.arg("-o").arg(output);
    run("clang", &mut command);
}

/// Runs a build step, panicking with the tool's output if it fails or is absent.
fn run(tool: &str, command: &mut Command) {
    let status = command
        .status()
        .unwrap_or_else(|error| panic!("Failed to run `{tool}` (is it installed?): {error}"));
    if !status.success() {
        panic!("`{tool}` failed with {status}.");
    }
}

/// Whether `output` must be rebuilt: it is missing, or `source` is newer than it.
fn is_stale(output: &Path, source: &Path) -> bool {
    let Ok(output_time) = modified(output) else {
        return true;
    };
    modified(source).map_or(true, |source_time| source_time > output_time)
}

fn modified(path: &Path) -> std::io::Result<SystemTime> {
    path.metadata()?.modified()
}
