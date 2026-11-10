// SPDX-License-Identifier: MIT

//! pinp's intermediate representation (IR): the program between the typed AST and the backends.
//!
//! Modelled on Open64's WHIRL: **one node type, several levels**, where a level is the set of
//! operations still allowed to appear. A function starts at the *structured* level — nested `if`s
//! and loops, whole-array operations — where pinp's own optimisations run. It ends at the *flat*
//! level — basic blocks and terminators, the control-flow graph — which analyses solve over and
//! backends translate. The level is part of the function's type ([`Function<Structured>`],
//! [`Function<Flat>`]), and [`verify`] checks which operations appear.
//!
//! Vocabulary:
//!
//! * Node: one [`Operation`] in a function's [`Arena`], named by its [`NodeId`]. Nodes form
//!   trees — every node has exactly one parent — so replacing a node in place changes one use.
//! * Statement / value: a statement is executed for its effect and sits in a body; a value is
//!   computed and sits under another node.
//! * Side table: data about nodes kept beside them, indexed by the same ids (types, spans, and
//!   whatever an analysis needs), so a node stays 16 bytes.
//! * Lowering: rewriting an operation into lower-level ones. Operations such as `Load` are allowed
//!   at both levels, so lowering can happen in small steps while control flow is still structured.

use rustc_hash::FxHashMap;

use crate::parser::{PinpType, StrId, SymId};

mod arena;
mod containers;
mod function;
mod operation;
mod print;
mod verify;

pub use arena::*;
pub(crate) use containers::typed_id;
pub use containers::{IdVec, List, ListPool, SideTable, TypedId};
pub use function::*;
pub use operation::*;
pub use print::*;
pub use verify::*;

#[cfg(test)]
mod tests;

typed_id!(
    /// A node in a function's [`Arena`].
    NodeId
);
typed_id!(
    /// A local variable, parameter, or compiler temporary of one function.
    LocalId
);
typed_id!(
    /// A global variable.
    GlobalId
);
typed_id!(
    /// A function of the program.
    FunctionId
);
typed_id!(
    /// A basic block of a flat function.
    BlockId
);
typed_id!(
    /// An interned [`PinpType`].
    TypeId
);
typed_id!(
    /// An interned [`Constant`].
    ConstantId
);

/// A literal value. Floats are kept as their bits, so NaN equals itself and `-0.0` differs from
/// `0.0` — what interning needs, and what the backends reproduce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Constant {
    Bool(bool),
    Int(i64),
    Float(u64),
    Str(StrId),
}

/// A whole program: its tables and functions, all at one level. The entry — the top-level
/// statements — is the last function.
#[derive(Debug)]
pub struct Program<S: Shape = Structured> {
    pub tables: ProgramTables,
    pub functions: IdVec<FunctionId, Function<S>>,
    pub entry: FunctionId,
}

/// Program-wide tables, shared by every function.
#[derive(Debug, Default)]
pub struct ProgramTables {
    pub types: TypeTable,
    pub constants: ConstantPool,
    pub globals: IdVec<GlobalId, Global>,
    pub signatures: IdVec<FunctionId, Signature>,
}

/// A global variable: a name first assigned at the top level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Global {
    pub name: SymId,
    pub type_id: TypeId,
}

/// A function's name and types. The entry has no name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    pub name: Option<SymId>,
    pub parameters: Vec<TypeId>,
    pub result: TypeId,
}

/// Each distinct [`PinpType`] once, under a 4-byte id.
#[derive(Debug, Default)]
pub struct TypeTable {
    types: IdVec<TypeId, PinpType>,
    ids: FxHashMap<PinpType, TypeId>,
}

impl TypeTable {
    /// The id of `pinp_type`, adding it on first use.
    pub fn intern(&mut self, pinp_type: PinpType) -> TypeId {
        *self
            .ids
            .entry(pinp_type)
            .or_insert_with(|| self.types.push(pinp_type))
    }

    pub fn get(&self, type_id: TypeId) -> PinpType {
        self.types[type_id]
    }
}

/// Each distinct [`Constant`] once, under a 4-byte id.
#[derive(Debug, Default)]
pub struct ConstantPool {
    constants: IdVec<ConstantId, Constant>,
    ids: FxHashMap<Constant, ConstantId>,
}

impl ConstantPool {
    /// The id of `constant`, adding it on first use.
    pub fn intern(&mut self, constant: Constant) -> ConstantId {
        *self
            .ids
            .entry(constant)
            .or_insert_with(|| self.constants.push(constant))
    }

    pub fn get(&self, constant_id: ConstantId) -> Constant {
        self.constants[constant_id]
    }
}

/// A local variable of one function. A compiler temporary has no name, and checkers never report
/// one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Local {
    pub name: Option<SymId>,
    pub type_id: TypeId,
}
