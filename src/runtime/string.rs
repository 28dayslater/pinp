// SPDX-License-Identifier: MIT

//! The immutable `str` runtime: `PinpStr`, a 16-byte small-string-optimised descriptor, and the
//! `extern "C"` ABI that JIT-compiled pinp code calls. Ported from the C++ prototype's
//! `ImmutableString`/`PinpStr`.
//!
//! `PinpStr` is the wire format shared between the compiler and the runtime. A string of up to 15
//! bytes lives entirely inline; a longer one is heap-allocated through the existing
//! [`pinp_alloc`]/[`pinp_free`] shim, so string allocations ride the same byte-exact bookkeeping the
//! leak check already uses — there is no separate string counter.
//!
//! Layout (the two modes share 16 bytes, discriminated on byte 15):
//!
//! ```text
//! inline (tag_len bit7 == 0):  buf[0..14] data, tag_len = length (0..15)
//! heap   (tag_len bit7 == 1):  ptr (8) | len (4) | cap (4, bit31 = is_heap)
//! ```
//!
//! In heap mode byte 15 is `cap`'s high byte, whose bit7 (is_heap) is always set; in inline mode it
//! is `tag_len`, whose bit7 is clear — so reading byte 15 discriminates the two.

use std::ffi::{CStr, c_char, c_void};

// The runtime allocator shim (src/runtime/shim.c), linked natively by build.rs. The heap path routes
// through it so `pinp_memory_info` accounts for string storage.
unsafe extern "C" {
    fn pinp_alloc(size: usize) -> *mut u8;
    fn pinp_free(ptr: *mut u8);
    fn pinp_runtime_error(message: *const c_char) -> !;
    fn mi_stats_print(out: *mut c_void);
}

/// The largest string stored inline; 16 bytes or more go to the heap.
const MAX_INLINE: usize = 15;

/// The largest heap string the descriptor can represent. The `cap` word spends its top two bits on
/// the `is_heap` and reserved-mutable flags, leaving a 30-bit length, so content longer than
/// 2^30 - 1 bytes cannot be stored without truncating the recorded length — it is a runtime error.
const MAX_STR_HEAP_LEN: usize = (1 << 30) - 1;

#[repr(C)]
#[derive(Clone, Copy)]
struct HeapRepr {
    ptr: *mut u8,
    len: u32,
    // bit31 = is_heap | bit30 = is_mutable (reserved) | bits[29:0] = capacity.
    cap: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct InlineRepr {
    buf: [u8; 15],
    // bit7 = is_heap | bit6 = is_mutable (reserved) | bits[3:0] = inline length.
    tag_len: u8,
}

/// A 16-byte immutable-string descriptor passed by value between the compiler and the runtime.
///
/// This is a plain wire struct, not an owner: heap storage is released explicitly via
/// [`pinp_str_free`], never by dropping a `PinpStr`. It is intentionally not `Copy` on the Rust side
/// so a value is moved rather than silently aliased; codegen passes it by value across the `{i64,
/// i64}` ABI.
#[repr(C)]
pub union PinpStr {
    heap: HeapRepr,
    inl: InlineRepr,
}

impl PinpStr {
    /// True when the string data lives on the heap rather than inline.
    pub fn is_heap(&self) -> bool {
        // Byte 15 (`tag_len` inline / `cap` high byte heap) discriminates the modes in both.
        unsafe { self.inl.tag_len & 0x80 != 0 }
    }

    /// The byte length of the string content (ASCII length == byte length).
    fn length(&self) -> usize {
        unsafe {
            if self.is_heap() {
                self.heap.len as usize
            } else {
                (self.inl.tag_len & 0x0f) as usize
            }
        }
    }

    /// The string content as a byte slice, borrowing for the lifetime of this descriptor.
    pub fn as_bytes(&self) -> &[u8] {
        unsafe {
            if self.is_heap() {
                std::slice::from_raw_parts(self.heap.ptr, self.heap.len as usize)
            } else {
                &self.inl.buf[..(self.inl.tag_len & 0x0f) as usize]
            }
        }
    }
}

/// Builds a `PinpStr` of `total` bytes, choosing inline or heap storage, and hands the destination
/// buffer to `write`. Centralises the inline/heap dispatch so every constructor shares it.
fn make_with(total: usize, write: impl FnOnce(&mut [u8])) -> PinpStr {
    if total <= MAX_INLINE {
        let mut inl = InlineRepr {
            buf: [0; 15],
            tag_len: total as u8, // bit7 clear: total <= 15
        };
        write(&mut inl.buf[..total]);
        PinpStr { inl }
    } else {
        // The 30-bit capacity field cannot record a length this large; reject it rather than
        // silently truncate. Diverges via longjmp, so only valid inside a `pinp_run` frame.
        if total > MAX_STR_HEAP_LEN {
            const TOO_LONG: &CStr = c"String length exceeds the maximum.";
            unsafe { pinp_runtime_error(TOO_LONG.as_ptr()) };
        }
        let ptr = unsafe { pinp_alloc(total) };
        // NOTE (deliberately untested): reaching this needs `pinp_alloc` to fail, and it cannot be
        // provoked from a test — the guard above caps a request at 1 GiB, which an overcommitting
        // Linux always grants. Faking it would mean an injection point in the allocator, which is
        // not worth carrying. Don't spend time trying to cover this line.
        if ptr.is_null() {
            const OUT_OF_MEMORY: &CStr = c"Out of memory.";
            unsafe { pinp_runtime_error(OUT_OF_MEMORY.as_ptr()) };
        }
        let dst = unsafe { std::slice::from_raw_parts_mut(ptr, total) };
        write(dst);
        PinpStr {
            heap: HeapRepr {
                ptr,
                len: total as u32,
                cap: 0x8000_0000 | total as u32, // total <= MAX_STR_HEAP_LEN, so it fits the 30 bits
            },
        }
    }
}

/// Builds a `PinpStr` from an existing byte slice.
fn make_from(bytes: &[u8]) -> PinpStr {
    make_with(bytes.len(), |dst| dst.copy_from_slice(bytes))
}

/// The empty inline string — also the reset state a freed descriptor is left in.
fn empty() -> PinpStr {
    make_with(0, |_| {})
}

/// A fixed stack buffer implementing [`core::fmt::Write`], so an integer can be formatted without the
/// transient heap `String` that `to_string` allocates. Being a plain local it is inherently
/// thread-safe — each call owns its own buffer, with no shared or static state. 24 bytes covers the
/// widest `i64` (`-9223372036854775808`, 20 chars); a write past the end would panic, which an `i64`
/// never reaches.
struct StackBuf {
    bytes: [u8; 24],
    len: usize,
}

impl std::fmt::Write for StackBuf {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        let end = self.len + text.len();
        self.bytes[self.len..end].copy_from_slice(text.as_bytes());
        self.len = end;
        Ok(())
    }
}

// ── extern "C" ABI ───────────────────────────────────────────────────────────

/// Constructs a `PinpStr` from a null-terminated C string.
///
/// # Safety
/// `s` must point to a valid null-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pinp_str_from_cstr(s: *const c_char) -> PinpStr {
    make_from(unsafe { CStr::from_ptr(s) }.to_bytes())
}

/// Frees heap storage (a no-op for an inline string) and resets `*s` to the empty inline string, so
/// a second free is harmless.
///
/// # Safety
/// `s` must point to a valid `PinpStr`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pinp_str_free(s: *mut PinpStr) {
    let s = unsafe { &mut *s };
    if s.is_heap() {
        unsafe { pinp_free(s.heap.ptr) };
    }
    *s = empty();
}

/// Returns a new `PinpStr` that is the concatenation of `a` and `b`, in a single allocation.
///
/// # Safety
/// `a` and `b` must point to valid `PinpStr`s.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pinp_str_concat(a: *const PinpStr, b: *const PinpStr) -> PinpStr {
    let (a, b) = unsafe { (&*a, &*b) };
    let (left, right) = (a.as_bytes(), b.as_bytes());
    make_with(left.len() + right.len(), |dst| {
        dst[..left.len()].copy_from_slice(left);
        dst[left.len()..].copy_from_slice(right);
    })
}

/// Returns a new `PinpStr` that is the concatenation of `parts[0..n]`, in a single allocation.
///
/// # Safety
/// `parts` must point to `n` valid `PinpStr`s (or be unused when `n == 0`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pinp_str_concat_n(parts: *const PinpStr, n: usize) -> PinpStr {
    if n == 0 {
        return empty();
    }
    let parts = unsafe { std::slice::from_raw_parts(parts, n) };
    let total: usize = parts.iter().map(|part| part.length()).sum();
    make_with(total, |dst| {
        let mut offset = 0;
        for part in parts {
            let bytes = part.as_bytes();
            dst[offset..offset + bytes.len()].copy_from_slice(bytes);
            offset += bytes.len();
        }
    })
}

/// The byte length of the string.
///
/// # Safety
/// `s` must point to a valid `PinpStr`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pinp_str_len(s: *const PinpStr) -> i64 {
    unsafe { &*s }.length() as i64
}

/// 1 if the two strings have equal content, 0 otherwise.
///
/// # Safety
/// `a` and `b` must point to valid `PinpStr`s.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pinp_str_eq(a: *const PinpStr, b: *const PinpStr) -> i32 {
    let (a, b) = unsafe { (&*a, &*b) };
    (a.as_bytes() == b.as_bytes()) as i32
}

/// A `memcmp`-style three-way comparison: negative, zero, or positive as `a` orders before, equal
/// to, or after `b` (a shorter prefix orders first).
///
/// # Safety
/// `a` and `b` must point to valid `PinpStr`s.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pinp_str_cmp(a: *const PinpStr, b: *const PinpStr) -> i32 {
    use std::cmp::Ordering::*;
    let (a, b) = unsafe { (&*a, &*b) };
    match a.as_bytes().cmp(b.as_bytes()) {
        Less => -1,
        Equal => 0,
        Greater => 1,
    }
}

/// Formats an `i64` as a decimal string.
///
/// # Safety
/// Always safe to call; `unsafe` only for ABI uniformity with the rest of the surface.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pinp_str_from_int(n: i64) -> PinpStr {
    // Format into a stack buffer rather than via `to_string`, whose transient heap `String` would be
    // pure waste — an `i64` always fits inline. The buffer is a per-call local, so this stays
    // thread-safe with no shared state.
    use std::fmt::Write;
    let mut buffer = StackBuf {
        bytes: [0; 24],
        len: 0,
    };
    let _ = write!(buffer, "{n}"); // formatting an i64 into 24 bytes cannot fail
    make_from(&buffer.bytes[..buffer.len])
}

/// Formats an `f64` as its shortest round-tripping decimal string.
///
/// Uses ryu rather than [`f64::to_string`]: it writes into a stack buffer (no transient heap
/// allocation), and it renders a whole float as `2.0` rather than `2`, so a float always reads as
/// one. Extreme magnitudes come out in scientific notation, which is what bounds the buffer.
///
/// # Safety
/// Always safe to call; `unsafe` only for ABI uniformity with the rest of the surface.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pinp_str_from_float(d: f64) -> PinpStr {
    // `format` (not `format_finite`) is the branch that spells NaN and the infinities.
    make_from(ryu::Buffer::new().format(d).as_bytes())
}

/// Prints mimalloc's allocation statistics to stderr (the `meminfo()` diagnostic built-in).
///
/// # Safety
/// Always safe to call; `unsafe` only for ABI uniformity with the rest of the surface.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pinp_meminfo() {
    unsafe { mi_stats_print(std::ptr::null_mut()) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    // Not in the regular suite (needs ~1 GiB RAM): a 2^30-byte string trips the heap-length guard,
    // which longjmps via pinp_runtime_error and is caught by pinp_run. Run on demand with
    // `cargo test --lib over_long_string_errors -- --ignored`.
    #[test]
    #[ignore]
    fn over_long_string_errors() {
        unsafe extern "C" {
            fn pinp_run(
                entry: unsafe extern "C" fn(*mut u8),
                result: *mut u8,
                error_out: *mut *const c_char,
            );
        }
        unsafe extern "C" fn trigger(_result: *mut u8) {
            let mut buf = vec![b'a'; (1usize << 30) + 1]; // 2^30 content bytes + a NUL
            *buf.last_mut().unwrap() = 0;
            let ptr = buf.as_ptr() as *const c_char;
            std::mem::forget(buf); // longjmp skips Drop; leak deliberately so there is none to skip
            let _ = unsafe { pinp_str_from_cstr(ptr) }; // diverges before returning
        }
        let mut result = [0u8; 16];
        let mut error: *const c_char = std::ptr::null();
        unsafe { pinp_run(trigger, result.as_mut_ptr(), &mut error) };
        assert!(!error.is_null());
        assert_eq!(
            unsafe { CStr::from_ptr(error) }.to_str().unwrap(),
            "String length exceeds the maximum."
        );
    }

    // Build a PinpStr from a Rust &str (test inputs carry no interior NUL).
    fn make(text: &str) -> PinpStr {
        let c = CString::new(text).unwrap();
        unsafe { pinp_str_from_cstr(c.as_ptr()) }
    }
    fn freed(mut s: PinpStr) {
        unsafe { pinp_str_free(&mut s) }
    }

    // --- SSO boundary ---------------------------------------------------------
    #[test]
    fn empty_is_inline() {
        let s = make("");
        assert!(!s.is_heap());
        assert_eq!(s.as_bytes(), b"");
        freed(s);
    }
    #[test]
    fn fifteen_bytes_is_inline() {
        let s = make("123456789012345"); // 15 == SSO max
        assert!(!s.is_heap());
        assert_eq!(s.as_bytes().len(), 15);
        freed(s);
    }
    #[test]
    fn sixteen_bytes_is_heap() {
        let s = make("1234567890123456"); // 16 crosses to heap
        assert!(s.is_heap());
        assert_eq!(s.as_bytes(), b"1234567890123456");
        freed(s);
    }

    // --- content round-trip ---------------------------------------------------
    #[test]
    fn inline_round_trips() {
        let s = make("hello");
        assert_eq!(s.as_bytes(), b"hello");
        freed(s);
    }
    #[test]
    fn long_heap_round_trips() {
        let text = "the quick brown fox jumps over the lazy dog";
        let s = make(text);
        assert!(s.is_heap());
        assert_eq!(s.as_bytes(), text.as_bytes());
        freed(s);
    }
    #[test]
    fn high_byte_stored_opaque() {
        let c = CString::new([0xC3u8, 0xA9].as_slice()).unwrap(); // "é" utf-8 bytes
        let s = unsafe { pinp_str_from_cstr(c.as_ptr()) };
        assert_eq!(s.as_bytes(), &[0xC3, 0xA9]);
        freed(s);
    }

    // --- concat ---------------------------------------------------------------
    #[test]
    fn concat_pair_inline() {
        let (a, b) = (make("foo"), make("bar"));
        let c = unsafe { pinp_str_concat(&a, &b) };
        assert_eq!(c.as_bytes(), b"foobar");
        assert!(!c.is_heap());
        freed(a);
        freed(b);
        freed(c);
    }
    #[test]
    fn concat_crosses_into_heap() {
        let (a, b) = (make("0123456789"), make("0123456789")); // 10 + 10 = 20
        let c = unsafe { pinp_str_concat(&a, &b) };
        assert!(c.is_heap());
        assert_eq!(c.as_bytes().len(), 20);
        freed(a);
        freed(b);
        freed(c);
    }
    #[test]
    fn concat_n_joins_in_order() {
        let parts = [make("a"), make("bb"), make("ccc")];
        let c = unsafe { pinp_str_concat_n(parts.as_ptr(), parts.len()) };
        assert_eq!(c.as_bytes(), b"abbccc");
        for p in parts {
            freed(p);
        }
        freed(c);
    }
    #[test]
    fn concat_n_with_empties() {
        let parts = [make(""), make("x"), make("")];
        let c = unsafe { pinp_str_concat_n(parts.as_ptr(), parts.len()) };
        assert_eq!(c.as_bytes(), b"x");
        for p in parts {
            freed(p);
        }
        freed(c);
    }
    #[test]
    fn concat_n_zero_parts_is_empty() {
        let c = unsafe { pinp_str_concat_n(std::ptr::null(), 0) };
        assert_eq!(c.as_bytes(), b"");
        freed(c);
    }

    // --- len / eq / cmp -------------------------------------------------------
    #[test]
    fn len_counts_bytes() {
        let s = make("héllo"); // 6 bytes (é is 2)
        assert_eq!(unsafe { pinp_str_len(&s) }, s.as_bytes().len() as i64);
        freed(s);
    }
    #[test]
    fn len_reads_the_heap_length_field() {
        // The inline and heap modes keep their length in different places, so a heap string's
        // length exercises a branch the inline cases never reach.
        let text = "x".repeat(MAX_INLINE + 25);
        let s = make(&text);
        assert!(s.is_heap());
        assert_eq!(unsafe { pinp_str_len(&s) }, text.len() as i64);
        freed(s);
    }
    #[test]
    fn meminfo_runs() {
        // Diagnostic only: it prints mimalloc's statistics to stderr and returns nothing, so the
        // contract worth checking is simply that calling it is safe. (It makes this test's output
        // noisy, which is what the function is for.)
        unsafe { pinp_meminfo() };
    }
    #[test]
    fn eq_compares_content_not_storage() {
        let (a, b, c) = (make("abc"), make("abc"), make("abd"));
        assert_eq!(unsafe { pinp_str_eq(&a, &b) }, 1);
        assert_eq!(unsafe { pinp_str_eq(&a, &c) }, 0);
        freed(a);
        freed(b);
        freed(c);
    }
    #[test]
    fn eq_differing_lengths() {
        let (a, b) = (make("ab"), make("abc"));
        assert_eq!(unsafe { pinp_str_eq(&a, &b) }, 0);
        freed(a);
        freed(b);
    }
    #[test]
    fn cmp_three_way_and_prefix() {
        let (a, b, p) = (make("abc"), make("abd"), make("ab"));
        assert!(unsafe { pinp_str_cmp(&a, &b) } < 0);
        assert!(unsafe { pinp_str_cmp(&b, &a) } > 0);
        assert_eq!(unsafe { pinp_str_cmp(&a, &a) }, 0);
        assert!(unsafe { pinp_str_cmp(&p, &a) } < 0); // prefix sorts first
        freed(a);
        freed(b);
        freed(p);
    }

    // --- numeric → str --------------------------------------------------------
    #[test]
    fn from_int_renders_decimal() {
        for (n, want) in [
            (0i64, "0"),
            (42, "42"),
            (-7, "-7"),
            (i64::MIN, "-9223372036854775808"),
        ] {
            let s = unsafe { pinp_str_from_int(n) };
            assert_eq!(s.as_bytes(), want.as_bytes());
            freed(s);
        }
    }
    #[test]
    fn from_float_round_trips_shortest() {
        // ryu's spelling: a whole float keeps its `.0`, and huge magnitudes go scientific.
        for (d, want) in [
            (1.5f64, "1.5"),
            (2.0, "2.0"),
            (-0.25, "-0.25"),
            (-0.0, "-0.0"),
            (1e300, "1e300"),
            (f64::NAN, "NaN"),
            (f64::INFINITY, "inf"),
            (f64::NEG_INFINITY, "-inf"),
        ] {
            let s = unsafe { pinp_str_from_float(d) };
            assert_eq!(s.as_bytes(), want.as_bytes());
            freed(s);
        }
    }

    // --- leak invariant ---------------------------------------------------------
    /// Mirrors `pinp_mem_info` from src/runtime/pinp_runtime.h.
    #[repr(C)]
    #[derive(Default)]
    struct MemInfo {
        outstanding_bytes: i64,
        allocation_count: i64,
        free_count: i64,
    }

    fn memory_info() -> MemInfo {
        unsafe extern "C" {
            fn pinp_memory_info(info: *mut MemInfo);
        }
        let mut info = MemInfo::default();
        unsafe { pinp_memory_info(&mut info) };
        info
    }

    #[test]
    fn heap_traffic_balances_through_the_shim() {
        // Every heap constructor must be matched by its free through the shim's byte-exact
        // bookkeeping. Deltas, not absolutes: other tests in this process share the counters
        // (each balances its own traffic, so the window is clean at the edges).
        let before = memory_info();

        let long = make("this string is longer than fifteen bytes"); // heap
        let left = make("0123456789"); // inline
        let right = make("0123456789"); // inline
        let joined = unsafe { pinp_str_concat(&left, &right) }; // 20 bytes: heap
        let parts = [make("12345678"), make("12345678")];
        let chained = unsafe { pinp_str_concat_n(parts.as_ptr(), parts.len()) }; // 16 bytes: heap

        freed(long);
        freed(left);
        freed(right);
        freed(joined);
        for part in parts {
            freed(part);
        }
        freed(chained);

        let after = memory_info();
        assert_eq!(after.allocation_count - before.allocation_count, 3);
        assert_eq!(
            after.allocation_count - before.allocation_count,
            after.free_count - before.free_count
        );
        assert_eq!(after.outstanding_bytes, before.outstanding_bytes);
    }

    #[test]
    fn concat_n_at_the_sso_boundary() {
        // 7 + 8 = 15 bytes stays inline; 8 + 8 = 16 crosses to the heap.
        let inline_parts = [make("1234567"), make("12345678")];
        let inline_sum = unsafe { pinp_str_concat_n(inline_parts.as_ptr(), inline_parts.len()) };
        assert!(!inline_sum.is_heap());
        assert_eq!(inline_sum.as_bytes(), b"123456712345678");

        let heap_parts = [make("12345678"), make("12345678")];
        let heap_sum = unsafe { pinp_str_concat_n(heap_parts.as_ptr(), heap_parts.len()) };
        assert!(heap_sum.is_heap());
        assert_eq!(heap_sum.as_bytes(), b"1234567812345678");

        for part in inline_parts.into_iter().chain(heap_parts) {
            freed(part);
        }
        freed(inline_sum);
        freed(heap_sum);
    }

    // --- free resets descriptor ----------------------------------------------
    #[test]
    fn free_resets_to_empty_inline() {
        let mut s = make("1234567890123456"); // heap
        unsafe { pinp_str_free(&mut s) };
        assert!(!s.is_heap());
        assert_eq!(s.as_bytes(), b"");
        unsafe { pinp_str_free(&mut s) }; // safe to free again
    }
}
