// SPDX-License-Identifier: MIT

use rustc_hash::FxHashMap;

// ---------------------------------------------------------------------------
// Types and interned ids
// ---------------------------------------------------------------------------

/// The scalar element types that an array may hold. A strict subset of [`PinpType`] so that
/// `PinpType::Array` stays `Copy` (it stores an `ArrayElementType`, not a boxed `PinpType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrayElementType {
    Bool,
    Int,
    Float,
}

impl From<ArrayElementType> for PinpType {
    fn from(element_type: ArrayElementType) -> PinpType {
        match element_type {
            ArrayElementType::Bool => PinpType::Bool,
            ArrayElementType::Int => PinpType::Int,
            ArrayElementType::Float => PinpType::Float,
        }
    }
}

/// The type of every expression and binding. `Void` is the type of a function with no declared
/// return type (and of a call to one); a value-typed expression is never `Void`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinpType {
    Bool,
    Int,
    Float,
    Void,
    Range,
    /// A 1D array of `n` elements of `ArrayElementType`, heap-allocated via `pinp_alloc`.
    Array(ArrayElementType, usize),
    /// A 2D matrix of `rows × cols` elements of `ArrayElementType`, heap-allocated, row-major.
    Matrix(ArrayElementType, usize, usize),
}

/// An interned identifier: an index into [`Ast::names`]. `Copy` and cheap to compare; the
/// backing text lives once in the interner, never re-stored per use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SymId(pub u32);

impl SymId {
    /// This id's underlying value — its index into [`Ast::names`].
    pub fn value(self) -> usize {
        self.0 as usize
    }
}

/// An index into the [`Ast::nodes`] arena. The node's inferred [`PinpType`] sits at the same
/// index in the parallel [`Ast::types`] vec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExprId(pub u32);

impl ExprId {
    /// This id's underlying value — its index into the parallel [`Ast::nodes`]/[`Ast::types`] arenas.
    pub fn value(self) -> usize {
        self.0 as usize
    }
}

// ---------------------------------------------------------------------------
// Operators
// ---------------------------------------------------------------------------

/// A binary operator. `IntDiv` and `Mod` are the `div`/`mod` keyword operators (integer-only);
/// `Pow` is `^` (right-associative). `Div` (`/`) always yields `Float`. The comparisons
/// (`Eq`..`Ge`) yield `Bool`; the logicals (`And`/`Or`/`Xor`) take and yield `Bool`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    IntDiv,
    Mod,
    Pow,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    And,
    Or,
    Xor,
}

/// A unary (prefix) operator: arithmetic negation (`-`) or logical `not`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

/// A compiler-known member that arrays and matrices expose via the `.member` syntax.
///
/// Sema resolves the raw member name to this variant (the one string-comparison site in the
/// compiler) and records it in [`Ast::builtin_members`]. Codegen pattern-matches the enum
/// exhaustively, so adding a new variant forces both sema and codegen to be updated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinMember {
    Len,
    Ndim,
    Rows,
    Cols,
}

impl BuiltinMember {
    /// Maps a raw member name to the enum variant, or `None` if the name is unrecognised.
    /// This is the single source of truth for which member names the language knows about.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "len" => Some(Self::Len),
            "ndim" => Some(Self::Ndim),
            "rows" => Some(Self::Rows),
            "cols" => Some(Self::Cols),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Expressions
// ---------------------------------------------------------------------------

/// An expression in the AST arena. Child expressions are referenced by [`ExprId`] index rather
/// than boxed, so a `Node` is a flat, `Clone` value and the whole tree lives in one `Vec`.
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Int(i64),
    Float(f64),
    Bool(bool),
    Var(SymId),    // bare name: a parameter/local, or a top-level global
    Global(SymId), // `::name`
    Unary {
        op: UnOp,
        operand: ExprId,
    },
    Bin {
        op: BinOp,
        lhs: ExprId,
        rhs: ExprId,
    },
    Call {
        callee: SymId,
        args: Vec<ExprId>,
    },
    /// `if`/`elif`/`else` — one node for both the block form and the one-line ternary (which is a
    /// single arm plus a mandatory `else_block`). Each arm's value is its body's trailing expression; the
    /// node yields a value only when `else_block` is present and every branch ends in one (see sema).
    If {
        arms: Vec<IfArm>,
        else_block: Option<Block>,
    },
    /// An integer range `start..stop[:step]`. `kind` records the operator (`..`/`..<`/`..>`); a
    /// `None` step defaults to ±1 at lowering. Its type is [`PinpType::Range`].
    Range {
        start: ExprId,
        stop: ExprId,
        step: Option<ExprId>,
        kind: RangeKind,
    },
    /// `value in range` — a `Bool` membership test.
    // TODO: consider renaming to `ValueInRange` once naming is settled.
    Membership {
        value: ExprId,
        range: ExprId,
    },
    /// `[e0, e1, …]` — a 1D array literal; all elements must share a common type.
    ArrayLiteral {
        elements: Vec<ExprId>,
    },
    /// `array[index]` — read one element of a 1D array.
    Index {
        array: ExprId,
        index: ExprId,
    },
    /// `object.member` — a built-in member access (e.g. `.len` on an array).
    Member {
        object: ExprId,
        member: SymId,
    },
    /// `[element for var[:type] in source]` — array comprehension.
    /// `var_type` defaults to the source range's element type; annotating `:float` promotes it.
    Comprehension {
        element: ExprId,
        var: SymId,
        var_type: PinpType,
        source: ExprId,
    },
    /// `[r0_e0, r0_e1, …; r1_e0, …; …]` — 2D matrix literal. Each inner `Vec` is one row; all
    /// rows must have the same length (checked by sema). Element type is deduced from all elements.
    MatrixLiteral {
        rows: Vec<Vec<ExprId>>,
    },
    /// `matrix[row, col]` — 2D index or slice. Each of `row` and `col` is one of:
    ///   • a scalar `Int` expression → index that dimension (runtime-checked)
    ///   • a `Node::Range` with literal bounds → slice that dimension (compile-time checked)
    ///   • `Node::FullExtent` → full extent of that dimension (`:` syntax, resolved by sema)
    Index2D {
        matrix: ExprId,
        row: ExprId,
        col: ExprId,
    },
    /// `:` in a 2D slice position — the full extent of that dimension. Valid only as the `row`
    /// or `col` child of `Node::Index2D`; sema resolves it to `0..rows-1` or `0..cols-1`.
    FullExtent,
}

/// The operator that introduced a [`Node::Range`]: `..` keeps both bounds, `..<`/`..>` drop the
/// stop bound (ascending / descending).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeKind {
    Inclusive,     // ..
    UpExclusive,   // ..<
    DownExclusive, // ..>
}

/// One `if`/`elif` arm: a condition and the body taken when it holds.
#[derive(Debug, Clone, PartialEq)]
pub struct IfArm {
    pub cond: ExprId,
    pub body: Block,
}

/// A named location that can be read or written: a bare name (current scope) or an
/// explicit `::` global. "Place" is the standard compiler term (Rust's "place expression")
/// for such a location — it appears on either side of `=`, not just as an assignment target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    Local(SymId),
    Global(SymId),
}

// ---------------------------------------------------------------------------
// Statements and items
// ---------------------------------------------------------------------------

/// A statement: an assignment, or an expression evaluated for its value.
///
/// `Assign` follows Python's `(target_list =)+ expr_list`: the `values` are evaluated once (in full,
/// before any store, so `a, b = b, a` swaps), then assigned positionally to every target group in
/// `target_lists`. Single `a = 1` is the degenerate `target_lists: [[a]], values: [rhs]`; a compound
/// assignment (`x += e`) desugars at parse time into that same single-target, single-value shape.
#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    Assign {
        target_lists: Vec<Vec<Place>>,
        values: Vec<ExprId>,
    },
    Expr(ExprId),
    /// Pre-test loop: run `body` while `cond` holds.
    While {
        cond: ExprId,
        body: Block,
    },
    /// Post-test (do–while) loop: run `body`, then repeat while `cond` holds (`until` ⇒ repeat
    /// while `cond` is *false*).
    Loop {
        body: Block,
        cond: ExprId,
        until: bool,
    },
    /// `for var in source <body>` — single-binder iteration. Source may be a range (`Int`
    /// loop variable), a 1D array (value only), or a 2D matrix (value only, row-major).
    For {
        var: SymId,
        source: ExprId,
        body: Block,
    },
    /// `for binders… in source <body>` — multi-binder array/matrix iteration.
    /// 2 binders + 1D array → (index, value); 3 binders + 2D matrix → (row, col, value).
    ForArray {
        binders: Vec<SymId>,
        source: ExprId,
        body: Block,
    },
    /// `arr[index] = value` or `arr[index] <op>= rhs` — write one element of a 1D array.
    /// For compound assignment `<op>=`, `compound_op` is `Some(op)` and `value` is the RHS;
    /// codegen loads the current element, applies `op`, and stores the result.
    IndexedAssign {
        target: Place,
        index: ExprId,
        value: ExprId,
        compound_op: Option<BinOp>,
    },
    /// `mat[row, col] = value` or `mat[row, col] <op>= rhs` — write one element of a 2D matrix.
    IndexedAssign2D {
        target: Place,
        row: ExprId,
        col: ExprId,
        value: ExprId,
        compound_op: Option<BinOp>,
    },
}

/// A function parameter: its interned name and resolved type.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: SymId,
    pub param_type: PinpType,
}

/// A block of statements with an optional trailing `result` expression — its value when used as
/// one (an `if` arm or a function body). A function body always carries a `result` (the parser
/// requires it); a control-flow body that ends in a statement has `result: None`.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub result: Option<ExprId>,
}

/// A parsed function definition: name, parameters, return type, and body.
#[derive(Debug, Clone, PartialEq)]
pub struct FuncDef {
    pub name: SymId,
    pub params: Vec<Param>,
    pub return_type: PinpType,
    pub body: Block,
}

/// One element at the top level of a program: a function definition, or a statement that runs in
/// the global scope.
#[derive(Debug, Clone, PartialEq)]
pub enum TopLevel {
    Func(FuncDef),
    Stmt(Stmt),
}

// ---------------------------------------------------------------------------
// The AST arena
// ---------------------------------------------------------------------------

/// A parsed program. Borrows the source for the lifetime `'src` (identifiers are slices into it).
///
/// Layout: `nodes`, `types`, and `builtin_members` are parallel arenas indexed by [`ExprId`];
/// `top_level` is the program in source order; `names` maps a [`SymId`] back to its text.
/// Returned by [`crate::parser::parse`] with `types` unpopulated and `builtin_members` all `None`;
/// [`crate::sema::analyze`] fills both in.
#[derive(Default)]
pub struct Ast<'src> {
    pub nodes: Vec<Node>,
    pub types: Vec<PinpType>,
    /// Parallel to `nodes`. For `Node::Member` expressions, sema writes the resolved
    /// [`BuiltinMember`] here; all other nodes remain `None`.
    pub builtin_members: Vec<Option<BuiltinMember>>,
    pub top_level: Vec<TopLevel>,
    pub names: Vec<&'src str>,
    symbols: FxHashMap<&'src str, SymId>,
}

impl<'src> Ast<'src> {
    /// Borrows the node at `expr_id`.
    pub fn node(&self, expr_id: ExprId) -> &Node {
        &self.nodes[expr_id.value()]
    }

    /// The type of the node at `expr_id` — valid only after [`crate::sema::analyze`] has run.
    pub fn type_of(&self, expr_id: ExprId) -> PinpType {
        self.types[expr_id.value()]
    }

    /// Pushes a node, returning its id. Type and built-in member are left as placeholders for
    /// sema to fill.
    pub(super) fn push(&mut self, node: Node) -> ExprId {
        let expr_id = ExprId(self.nodes.len() as u32);
        self.nodes.push(node);
        self.types.push(PinpType::Void);
        self.builtin_members.push(None);
        expr_id
    }

    /// Records the resolved [`BuiltinMember`] kind for a `Node::Member` expression. Called by
    /// sema after validating the member name and object type.
    pub fn set_builtin_member(&mut self, expr_id: ExprId, kind: BuiltinMember) {
        self.builtin_members[expr_id.value()] = Some(kind);
    }

    /// The resolved [`BuiltinMember`] kind for `expr_id`. `None` for non-`Node::Member` nodes.
    /// Valid only after [`crate::sema::analyze`] has run.
    pub fn builtin_member_of(&self, expr_id: ExprId) -> Option<BuiltinMember> {
        self.builtin_members[expr_id.value()]
    }

    pub(super) fn intern(&mut self, name: &'src str) -> SymId {
        if let Some(&sym_id) = self.symbols.get(name) {
            return sym_id;
        }
        let sym_id = SymId(self.names.len() as u32);
        self.names.push(name);
        self.symbols.insert(name, sym_id);
        sym_id
    }
}

/// The expression that *reads* a [`Place`]: `x` -> `Var`, `::g` -> `Global`. Used to build the
/// `place` operand when desugaring `place <op>= e` into `place = place <op> e`.
impl From<Place> for Node {
    fn from(place: Place) -> Node {
        match place {
            Place::Local(sym_id) => Node::Var(sym_id),
            Place::Global(sym_id) => Node::Global(sym_id),
        }
    }
}
