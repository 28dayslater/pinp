[Human]

# Runtime allocator (mimalloc)

Infrastructure preparation, ahead of the features (strings, arrays, …) that will need to allocate.

Like the C++ prototype, the runtime uses mimalloc. The build process builds mimalloc and links it
into pinp, exposing only the handful of entry points pinp actually needs: allocate, free, and a small
amount of memory information. mimalloc's own symbol surface is kept private.

Future code that deals with strings, arrays, etc. will be able to issue allocation/free requests
against this runtime.

Design decisions taken up front:

- **Exported surface:** a thin `pinp_*` C shim (`pinp_alloc` / `pinp_free` / `pinp_memory_info`)
  wraps mimalloc, so pinp's codegen targets stable names of our own and never mimalloc's API
  directly. All `mi_*` symbols stay private.
- **mimalloc source:** vendored as a git submodule under `third_party/mimalloc`, pinned to a tag
  (v2.1.7, as the prototype used).
- **`pinp_memory_info`:** the minimum needed to detect leaks and print a simple summary (an upcoming
  built-in) — outstanding bytes plus allocation/free counts.

[AI]

## Goal

Stand up the pinp runtime allocator as build-and-link infrastructure: vendor mimalloc, wrap it in a
tiny `pinp_*` shim, build both into the pinp binary, and make the shim's three entry points
resolvable from JIT-compiled pinp code. No language-visible feature ships this iteration — this is
the foundation later allocation (strings, arrays) builds on.

## The TLS constraint (why the allocator is native, not JIT-uploaded)

The first cut compiled mimalloc + shim to a single self-contained `.bc`, internalized everything but
the `pinp_*` surface, and uploaded that module into the JIT on startup. It segfaulted on the first
allocation.

Cause: mimalloc keeps its per-thread heap in a `thread_local` global (`_mi_heap_default`). When that
TLS is accessed from **JIT-compiled** code, the backend emits native `%fs:`/`TPOFF` relocations that
ORC's linker cannot resolve. The only codegen fix is emulated TLS (`__emutls_get_address`), but that
is a `TargetMachine` option the LLVM **C API does not expose** (no `EmulatedTLS` setter on
`LLVMTargetMachineOptions`, none on `LLVMCreateTargetMachine`). So "run mimalloc inside the JIT"
is not reachable on the C API we use.

The prototype never hit this because it linked mimalloc into the host binary — mimalloc ran as
ordinary native code, its TLS set up normally, and JIT-compiled pinp code merely *called* it. We do
the same. The upload-to-JIT mechanism itself is fine; it is specifically mimalloc's own TLS-touching
code running JITed that fails. (If a JIT-resident runtime is ever wanted, the shim — which has no TLS
of its own — could be uploaded while still calling a natively-resident mimalloc. Not needed now.)

## Build pipeline ([build.rs](../build.rs))

Runs only under the `llvm` feature (`CARGO_FEATURE_LLVM`); a `--no-default-features` front-end build
needs no allocator and no C toolchain. Tools: `clang` and `ar` (LLVM 22 is already a project
prerequisite).

1. Compile mimalloc's `src/static.c` amalgamation to a native object (`-O2 -fPIC`). Cached: rebuilt
   only when missing or older than its source — the slow step.
2. Compile `runtime/shim.c` likewise.
3. `ar crs` both into `libpinp_runtime.a` in `OUT_DIR`.
4. Emit link directives:
   - `rustc-link-lib=static:+whole-archive=pinp_runtime` — the binary never references the runtime
     directly (the JIT resolves it at run time), so without `whole-archive` the linker would drop it.
   - `--export-dynamic-symbol=pinp_{alloc,free,memory_info}` — publish only those three into the
     dynamic symbol table, so the JIT's `dlsym`-based search generator can find them while every
     `mi_*` symbol stays private.

`rerun-if-changed` covers `static.c`, the shim, the header, and `build.rs`, so an untouched tree
never re-runs the script.

## Runtime shim ([runtime/](../runtime))

- [pinp_runtime.h](../runtime/pinp_runtime.h) — the exported ABI: the three functions and a
  `pinp_mem_info { outstanding_bytes, allocation_count, free_count }` struct.
- [shim.c](../runtime/shim.c) — wraps `mi_malloc`/`mi_free` and keeps byte-exact bookkeeping. It
  charges/credits `mi_usable_size(ptr)` (so `pinp_free` needs no size argument), and the counts feed
  leak detection and the planned memory-summary built-in. Plain counters, no atomics: pinp's JIT runs
  single-threaded.

## JIT ([src/codegen/jit.rs](../src/codegen/jit.rs))

`Jit::new` attaches a dynamic-library search generator
(`LLVMOrcCreateDynamicLibrarySearchGeneratorForProcess`) to the main dylib, so symbols pinp code
references resolve against the host process — the `pinp_*` runtime now, libc later as needed. This is
the path JIT-compiled allocation calls will take.

## Test plan (TDD: red → green → refactor)

- `runtime_allocator_is_reachable_from_the_jit` ([src/codegen/tests.rs](../src/codegen/tests.rs)):
  look the three symbols up through the JIT (exercising the search generator), allocate, assert the
  bookkeeping (`allocation_count == 1`, `outstanding_bytes >= request`), free, assert it returns to
  zero with `free_count == 1`. This is what proved the native cut works after the bitcode cut
  segfaulted.

## Deferred

- Codegen does not yet emit any `pinp_alloc`/`pinp_free` calls — that arrives with the first
  heap-allocating feature (strings/arrays).
- The memory-summary built-in that consumes `pinp_memory_info`.
