// SPDX-License-Identifier: MIT

pub mod lexer;
pub mod parser;
pub mod sema;

#[cfg(feature = "llvm")]
pub mod codegen;

#[cfg(feature = "llvm")]
pub mod runtime;
