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

#endif // PINP_RUNTIME_H
