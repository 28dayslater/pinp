// SPDX-License-Identifier: MIT

pub mod lexer;
pub mod parser;

#[cfg(feature = "llvm")]
pub mod codegen;
