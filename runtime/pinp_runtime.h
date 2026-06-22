// SPDX-License-Identifier: MIT
//
// The pinp runtime ABI: the stable surface that JIT-compiled pinp code links
// against. Everything here is compiled (together with mimalloc) into a single
// self-contained bitcode module; only these three symbols stay externally
// visible, so codegen targets `pinp_*` names and never mimalloc's directly.

#ifndef PINP_RUNTIME_H
#define PINP_RUNTIME_H

#include <stddef.h>
#include <stdint.h>

// A snapshot of the runtime's allocation bookkeeping. `outstanding_bytes`
// returning to zero (and the two counts agreeing) is what a leak check asserts;
// the counts also feed the upcoming memory-summary built-in.
typedef struct {
    int64_t outstanding_bytes;
    int64_t allocation_count;
    int64_t free_count;
} pinp_mem_info;

// Allocates `size` bytes, or returns null on failure.
void *pinp_alloc(size_t size);

// Frees a block previously returned by pinp_alloc. A null pointer is ignored.
void pinp_free(void *ptr);

// Writes the current allocation bookkeeping into `out`.
void pinp_memory_info(pinp_mem_info *out);

// Reports a runtime error and unwinds the JIT call stack via longjmp. Never returns.
// Must only be called from within a pinp_run invocation.
void pinp_runtime_error(const char *message);

// Calls `entry(result)` inside a setjmp frame. If `entry` returns normally `*error_out`
// is left null. If JIT code calls `pinp_runtime_error`, the longjmp lands here and
// `*error_out` is set to the error message instead.
void pinp_run(void (*entry)(void *), void *result, const char **error_out);

#endif // PINP_RUNTIME_H
