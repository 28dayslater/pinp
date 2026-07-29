// SPDX-License-Identifier: MIT

pub mod analysis;
pub mod lexer;
pub mod parser;
pub mod sema;

#[cfg(feature = "llvm")]
pub mod codegen;

#[cfg(feature = "llvm")]
pub mod runtime;
