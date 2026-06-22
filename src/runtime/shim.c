// SPDX-License-Identifier: MIT
//
// The pinp runtime shim over mimalloc. It owns the `pinp_*` ABI (see
// pinp_runtime.h) and keeps byte-exact allocation bookkeeping: routing every
// allocation through here lets a leak check be exact, where mimalloc's own
// process stats are only page-granular.

#include "pinp_runtime.h"
#include <mimalloc.h>
#include <setjmp.h>

// pinp's JIT executes single-threaded, so plain counters are enough — no atomics.
static int64_t outstanding_bytes = 0;
static int64_t allocation_count = 0;
static int64_t free_count = 0;

void *pinp_alloc(size_t size) {
    void *ptr = mi_malloc(size);
    if (ptr) {
        // Charge the usable size, not the request, so pinp_free can credit the
        // same amount back without being told the original size.
        outstanding_bytes += (int64_t)mi_usable_size(ptr);
        allocation_count += 1;
    }
    return ptr;
}

void pinp_free(void *ptr) {
    if (ptr) {
        outstanding_bytes -= (int64_t)mi_usable_size(ptr);
        free_count += 1;
    }
    mi_free(ptr);
}

void pinp_memory_info(pinp_mem_info *out) {
    out->outstanding_bytes = outstanding_bytes;
    out->allocation_count = allocation_count;
    out->free_count = free_count;
}

// ---------------------------------------------------------------------------
// Runtime error recovery
// ---------------------------------------------------------------------------

static jmp_buf  pinp_jmpbuf;
static const char *pinp_pending_error = NULL;

void pinp_runtime_error(const char *message) {
    pinp_pending_error = message;
    longjmp(pinp_jmpbuf, 1);
}

void pinp_run(void (*entry)(void *), void *result, const char **error_out) {
    pinp_pending_error = NULL;
    *error_out = NULL;
    if (setjmp(pinp_jmpbuf) == 0) {
        entry(result);
    } else {
        *error_out = pinp_pending_error;
    }
}
