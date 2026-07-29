// SPDX-License-Identifier: MIT

//! The pinp runtime: native host code that JIT-compiled pinp programs call into. The allocator
//! shim and mimalloc are C (built and linked by `build.rs`); the `str` runtime is Rust here. Both
//! are resolved by the JIT's process-symbol search generator, the same path that finds
//! `pinp_alloc`.

pub mod string;
