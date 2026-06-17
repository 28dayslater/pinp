// SPDX-License-Identifier: MIT

//! Semantic analysis: the pass between [`crate::parser`] and code generation.
//!
//! The parser produces a structural [`Ast`] with an unpopulated `types` arena. [`analyze`] walks
//! it, **resolves names** against a scope stack, **infers** every expression's [`PinpType`]
//! (writing it back into the arena), and **checks** the semantic rules — reporting the first
//! [`SemaError`]. Codegen then consumes the now-typed AST.
//!
//! Scoping mirrors the language: `scopes[0]` is global; a function pushes a frame seeded with its
//! parameters. A bare name resolves against the innermost frame only (never reaching globals from
//! inside a function); `::name` always targets the global frame. Function signatures are recorded
//! *after* each definition is analysed, giving define-before-use with no recursion.

use rustc_hash::FxHashMap;

use crate::parser::{
    Ast, BinOp, Block, ExprId, FuncDef, Node, PinpType, Place, Stmt, SymId, TopLevel, UnOp,
};

/// A semantic failure. pinp is fail-fast: the first error stops the pass.
#[derive(Debug, Clone, PartialEq)]
pub enum SemaError {
    /// A name used but not bound: unknown variable, global, or function.
    UnknownSymbol(String),
    /// A type error: a mismatch, a bad promotion, or an operation on `Void`.
    Type(String),
}

/// `from` is assignable to `to` if identical, or by widening up the promotion lattice
/// `Bool -> Int -> Float`. Narrowing (e.g. `Float -> Int`, anything `-> Bool`) is never implicit.
fn assignable(from: PinpType, to: PinpType) -> bool {
    use PinpType::*;
    from == to || matches!((from, to), (Bool, Int) | (Bool, Float) | (Int, Float))
}

/// The wider of two types under the `Bool -> Int -> Float` lattice — the common type an `if`'s
/// branches join to. `None` when neither is assignable to the other (e.g. a `Void` branch), which
/// makes the whole `if` `Void` and so usable only as a statement.
fn join(a: PinpType, b: PinpType) -> Option<PinpType> {
    if assignable(a, b) {
        Some(b)
    } else if assignable(b, a) {
        Some(a)
    } else {
        None
    }
}

/// Whether `t` participates in arithmetic/comparison, i.e. is not `Void`.
fn numeric(t: PinpType) -> bool {
    t != PinpType::Void
}

/// Whether `t` promotes to `Int` (rather than forcing a `Float` result): `Bool` or `Int`.
fn int_like(t: PinpType) -> bool {
    matches!(t, PinpType::Bool | PinpType::Int)
}

#[derive(Clone)]
struct Signature {
    params: Vec<PinpType>,
    return_type: PinpType,
}

/// Infers types and checks the semantic rules, filling the AST's `types` arena in place.
pub fn analyze(ast: &mut Ast) -> Result<(), SemaError> {
    // Borrow the read-only structure and the writable `types` arena as disjoint fields.
    let Ast {
        nodes,
        types,
        top_level,
        names,
        ..
    } = ast;
    let mut analyzer = Analyzer {
        nodes,
        names,
        types,
        scopes: vec![FxHashMap::default()], // global frame
        fn_base: 0,
        funcs: FxHashMap::default(),
    };
    for item in top_level.iter() {
        match item {
            TopLevel::Stmt(stmt) => analyzer.analyze_stmt(stmt)?,
            TopLevel::Func(func) => analyzer.analyze_func(func)?,
        }
    }
    Ok(())
}

struct Analyzer<'a, 'src> {
    nodes: &'a [Node],
    names: &'a [&'src str],
    types: &'a mut Vec<PinpType>,
    scopes: Vec<FxHashMap<SymId, PinpType>>,
    // Index of the current function's base frame; bare-name resolution searches `scopes[fn_base..]`
    // and never reaches the global frame from inside a function. `0` at the top level, where the
    // global frame *is* the base.
    fn_base: usize,
    funcs: FxHashMap<SymId, Signature>,
}

impl Analyzer<'_, '_> {
    fn name(&self, s: SymId) -> &str {
        self.names[s.0 as usize]
    }

    fn analyze_func(&mut self, func: &FuncDef) -> Result<(), SemaError> {
        let mut frame = FxHashMap::default();
        for p in &func.params {
            // `void` is the no-return marker, not a value type — rejecting it here is what keeps a
            // `Void` value from ever entering an expression.
            if p.param_type == PinpType::Void {
                return Err(SemaError::Type(format!(
                    "Function argument `{}` cannot be void.",
                    self.name(p.name)
                )));
            }
            frame.insert(p.name, p.param_type); // duplicate params already rejected by the parser
        }
        let old_base = self.fn_base;
        self.scopes.push(frame);
        self.fn_base = self.scopes.len() - 1;
        for stmt in &func.body.stmts {
            self.analyze_stmt(stmt)?;
        }
        let result = func
            .body
            .result
            .expect("a function body always ends in a result expression");
        let result_type = self.analyze_expr(result)?;
        self.scopes.pop();
        self.fn_base = old_base;

        if !assignable(result_type, func.return_type) {
            return Err(SemaError::Type(format!(
                "Function `{}` body yields {result_type:?} but is declared {:?}.",
                self.name(func.name),
                func.return_type
            )));
        }
        // Register only now: a call can reach a function only after its definition (so no forward
        // references and no recursion).
        self.funcs.insert(
            func.name,
            Signature {
                params: func.params.iter().map(|p| p.param_type).collect(),
                return_type: func.return_type,
            },
        );
        Ok(())
    }

    fn analyze_stmt(&mut self, stmt: &Stmt) -> Result<(), SemaError> {
        match stmt {
            Stmt::Expr(e) => {
                self.analyze_expr(*e)?;
                Ok(())
            }
            Stmt::Assign { target, rhs } => {
                let rhs_type = self.analyze_expr(*rhs)?;
                if rhs_type == PinpType::Void {
                    return Err(SemaError::Type("Cannot assign a void value.".into()));
                }
                match *target {
                    Place::Global(s) => match self.scopes[0].get(&s).copied() {
                        None => {
                            return Err(SemaError::UnknownSymbol(format!(
                                "Unknown global `::{}`.",
                                self.name(s)
                            )))
                        }
                        Some(existing) => self.check_assignable(rhs_type, existing)?,
                    },
                    Place::Local(s) => match self.lookup_assign_target(s) {
                        // Mutate the nearest enclosing binding if one exists (so a conditional
                        // update or a loop counter alters the outer variable)...
                        Some(existing) => self.check_assignable(rhs_type, existing)?,
                        // ...otherwise introduce a fresh local in the innermost (current body)
                        // scope, where it does not escape.
                        None => {
                            self.scopes.last_mut().unwrap().insert(s, rhs_type);
                        }
                    },
                }
                Ok(())
            }
            Stmt::While { cond, body } => {
                self.check_condition(*cond)?;
                self.analyze_block(body)?;
                Ok(())
            }
            Stmt::Loop { body, cond, .. } => {
                self.analyze_block(body)?;
                self.check_condition(*cond)?;
                Ok(())
            }
        }
    }

    /// Analyses a control-flow body in its own scope, returning the type of its trailing result
    /// expression (or `None` when it ends in a statement). The scope is popped on the way out, so a
    /// name first assigned here does not escape.
    fn analyze_block(&mut self, block: &Block) -> Result<Option<PinpType>, SemaError> {
        self.scopes.push(FxHashMap::default());
        for stmt in &block.stmts {
            self.analyze_stmt(stmt)?;
        }
        let result_type = match block.result {
            Some(e) => Some(self.analyze_expr(e)?),
            None => None,
        };
        self.scopes.pop();
        Ok(result_type)
    }

    /// A condition (`if`/`while`/`loop`) must be `Bool` — there is no truthiness.
    fn check_condition(&mut self, cond: ExprId) -> Result<(), SemaError> {
        let t = self.analyze_expr(cond)?;
        if t == PinpType::Bool {
            Ok(())
        } else {
            Err(SemaError::Type(format!(
                "Condition must be Bool, got {t:?}."
            )))
        }
    }

    /// Finds the type of the nearest enclosing binding of `s` (innermost first, down to the
    /// function base), or `None` if `s` is bound nowhere in scope — meaning an assignment should
    /// introduce it as a new local.
    fn lookup_assign_target(&self, s: SymId) -> Option<PinpType> {
        self.scopes[self.fn_base..]
            .iter()
            .rev()
            .find_map(|frame| frame.get(&s).copied())
    }

    fn check_assignable(&self, from: PinpType, to: PinpType) -> Result<(), SemaError> {
        if assignable(from, to) {
            Ok(())
        } else {
            Err(SemaError::Type(format!(
                "Assignment yields {from:?}, not assignable to {to:?}."
            )))
        }
    }

    /// Infers the type of `e`, records it in the arena, and returns it.
    fn analyze_expr(&mut self, e: ExprId) -> Result<PinpType, SemaError> {
        let ty = match self.nodes[e.0 as usize].clone() {
            Node::Int(_) => PinpType::Int,
            Node::Float(_) => PinpType::Float,
            Node::Bool(_) => PinpType::Bool,
            Node::Var(s) => self.lookup_local(s)?,
            Node::Global(s) => self.lookup_global(s)?,
            Node::Unary { op, operand } => {
                let t = self.analyze_expr(operand)?;
                self.unary_type(op, t)?
            }
            Node::Bin { op, lhs, rhs } => {
                let lt = self.analyze_expr(lhs)?;
                let rt = self.analyze_expr(rhs)?;
                self.bin_type(op, lt, rt)?
            }
            Node::Call { callee, args } => self.call_type(callee, &args)?,
            Node::If { arms, els } => self.if_type(&arms, els.as_ref())?,
        };
        self.types[e.0 as usize] = ty;
        Ok(ty)
    }

    /// Types an `if`: each condition must be `Bool`; the node's type is the join of every branch's
    /// result, but only when an `else` is present and every branch (arms and `else`) ends in an
    /// expression. Otherwise it is `Void` — usable as a statement, but not as a value.
    fn if_type(
        &mut self,
        arms: &[crate::parser::IfArm],
        els: Option<&Block>,
    ) -> Result<PinpType, SemaError> {
        let mut branch_types = Vec::with_capacity(arms.len());
        for arm in arms {
            self.check_condition(arm.cond)?;
            branch_types.push(self.analyze_block(&arm.body)?);
        }
        let Some(els) = els else {
            return Ok(PinpType::Void);
        };
        let else_type = self.analyze_block(els)?;

        // A missing branch result, or branches that do not share a common type, leave the `if`
        // valueless (`Void`).
        let mut acc = match else_type {
            Some(t) => t,
            None => return Ok(PinpType::Void),
        };
        for bt in branch_types {
            match bt.and_then(|t| join(acc, t)) {
                Some(j) => acc = j,
                None => return Ok(PinpType::Void),
            }
        }
        Ok(acc)
    }

    fn lookup_local(&self, s: SymId) -> Result<PinpType, SemaError> {
        // Reading and assigning resolve a bare name the same way — outward to the function base —
        // so both share `lookup_assign_target`.
        self.lookup_assign_target(s)
            .ok_or_else(|| SemaError::UnknownSymbol(format!("Unknown symbol `{}`.", self.name(s))))
    }

    fn lookup_global(&self, s: SymId) -> Result<PinpType, SemaError> {
        self.scopes[0].get(&s).copied().ok_or_else(|| {
            SemaError::UnknownSymbol(format!("Unknown global `::{}`.", self.name(s)))
        })
    }

    fn unary_type(&self, op: UnOp, t: PinpType) -> Result<PinpType, SemaError> {
        match op {
            // Arithmetic negation: `Bool` promotes to `Int` (like any arithmetic use of a bool).
            UnOp::Neg => match t {
                PinpType::Void => Err(SemaError::Type("Unary minus on a void value.".into())),
                PinpType::Bool => Ok(PinpType::Int),
                other => Ok(other),
            },
            UnOp::Not => {
                if t == PinpType::Bool {
                    Ok(PinpType::Bool)
                } else {
                    Err(SemaError::Type(format!(
                        "`not` requires a Bool operand, got {t:?}."
                    )))
                }
            }
        }
    }

    fn bin_type(&self, op: BinOp, lt: PinpType, rt: PinpType) -> Result<PinpType, SemaError> {
        use BinOp::*;
        use PinpType::*;
        // Logicals take and yield Bool only — no truthiness.
        if matches!(op, And | Or | Xor) {
            return if lt == Bool && rt == Bool {
                Ok(Bool)
            } else {
                Err(SemaError::Type(format!(
                    "`{op:?}` requires Bool operands, got {lt:?} and {rt:?}."
                )))
            };
        }
        // Arithmetic and comparison both require numeric (non-void) operands.
        if !numeric(lt) || !numeric(rt) {
            return Err(SemaError::Type("Operation on a void value.".into()));
        }
        Ok(match op {
            Eq | Ne | Lt | Gt | Le | Ge => Bool,
            Add | Sub | Mul | Pow => {
                if int_like(lt) && int_like(rt) {
                    Int
                } else {
                    Float
                }
            }
            Div => Float,
            IntDiv | Mod => {
                if int_like(lt) && int_like(rt) {
                    Int
                } else {
                    return Err(SemaError::Type(format!("{op:?} requires Int operands.")));
                }
            }
            And | Or | Xor => unreachable!("handled above"),
        })
    }

    fn call_type(&mut self, callee: SymId, args: &[ExprId]) -> Result<PinpType, SemaError> {
        let sig = self.funcs.get(&callee).cloned().ok_or_else(|| {
            SemaError::UnknownSymbol(format!(
                "Call to undefined function `{}`.",
                self.name(callee)
            ))
        })?;
        if args.len() != sig.params.len() {
            return Err(SemaError::Type(format!(
                "Function `{}` expects {} argument(s), got {}.",
                self.name(callee),
                sig.params.len(),
                args.len()
            )));
        }
        for (i, (&arg, &pt)) in args.iter().zip(sig.params.iter()).enumerate() {
            let at = self.analyze_expr(arg)?;
            if !assignable(at, pt) {
                return Err(SemaError::Type(format!(
                    "Argument {} of `{}`: {at:?} not assignable to {pt:?}.",
                    i + 1,
                    self.name(callee)
                )));
            }
        }
        Ok(sig.return_type)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{parse, Node, Stmt, TopLevel};
    use indoc::indoc;

    /// Parse + analyze, returning the typed AST (panicking on any error).
    fn analyzed(src: &str) -> Ast<'_> {
        let mut ast = parse(src).expect("source should parse");
        analyze(&mut ast).expect("source should analyze");
        ast
    }

    /// Parse (expecting success) then analyze, returning the semantic error.
    fn sema_error(src: &str) -> SemaError {
        let mut ast = parse(src).expect("source should parse");
        analyze(&mut ast).expect_err("analysis should fail")
    }

    /// Type of the program's final top-level expression.
    fn root_type(src: &str) -> PinpType {
        let ast = analyzed(src);
        match ast.top_level.last().unwrap() {
            TopLevel::Stmt(Stmt::Expr(e)) => ast.type_of(*e),
            TopLevel::Stmt(Stmt::Assign { rhs, .. }) => ast.type_of(*rhs),
            other => panic!("program does not end in an expression: {other:?}"),
        }
    }

    // --- type inference ------------------------------------------------------------------

    #[test]
    fn arithmetic_type_rules() {
        assert_eq!(root_type("2 + 3"), PinpType::Int);
        assert_eq!(root_type("10 / 4"), PinpType::Float);
        assert_eq!(root_type("10 div 4"), PinpType::Int);
        assert_eq!(root_type("7 mod 3"), PinpType::Int);
        assert_eq!(root_type("2 ^ 10"), PinpType::Int);
        assert_eq!(root_type("2.0 ^ 10"), PinpType::Float);
        assert_eq!(root_type("-3.14"), PinpType::Float);
    }

    #[test]
    fn int_promotes_to_float() {
        assert_eq!(root_type("a = 2\n2.0 * a"), PinpType::Float);
    }

    #[test]
    fn assignment_then_reference() {
        let ast = analyzed("a = 2 + 3\na * a");
        let TopLevel::Stmt(Stmt::Expr(e)) = ast.top_level.last().unwrap() else {
            panic!("expected an expression statement");
        };
        assert!(matches!(ast.node(*e), Node::Bin { op: BinOp::Mul, .. }));
        assert_eq!(ast.type_of(*e), PinpType::Int);
    }

    #[test]
    fn reassignment_to_incompatible_type_is_error() {
        // Decision (0004): assignment is checked uniformly — re-binding to a non-assignable
        // type is an error, just like a compound assignment. `Float` is not assignable to `Int`.
        assert!(matches!(sema_error("a = 1\na = 2.0"), SemaError::Type(_)));
    }

    #[test]
    fn reassignment_promotes_into_float_slot() {
        // `a` is Float; re-assigning an Int value is fine (Int promotes), and `a` stays Float.
        assert_eq!(root_type("a = 1.0\na = 2\na"), PinpType::Float);
    }

    // --- bool, comparisons & logicals ----------------------------------------------------

    #[test]
    fn bool_literal_and_logicals() {
        assert_eq!(root_type("true"), PinpType::Bool);
        assert_eq!(root_type("true and false"), PinpType::Bool);
        assert_eq!(root_type("true or false xor true"), PinpType::Bool);
        assert_eq!(root_type("not true"), PinpType::Bool);
    }

    #[test]
    fn comparisons_yield_bool() {
        assert_eq!(root_type("1 < 2"), PinpType::Bool);
        assert_eq!(root_type("1.0 >= 2"), PinpType::Bool);
        assert_eq!(root_type("true == false"), PinpType::Bool);
        assert_eq!(root_type("1 < 2 < 3"), PinpType::Bool); // chained
    }

    #[test]
    fn bool_promotes_in_arithmetic() {
        assert_eq!(root_type("true + false"), PinpType::Int); // bool -> int
        assert_eq!(root_type("true + 1"), PinpType::Int);
        assert_eq!(root_type("true + 1.0"), PinpType::Float);
        assert_eq!(root_type("-true"), PinpType::Int); // unary minus promotes bool to int
    }

    #[test]
    fn bool_assignable_to_numeric_slot() {
        // bool flows into an int/float return slot, but not the reverse.
        assert_eq!(
            root_type(indoc! {"
                f(a: bool): int is a
                f(true)
            "}),
            PinpType::Int
        );
        assert!(matches!(
            sema_error("f(a: int): bool is a"),
            SemaError::Type(_)
        ));
    }

    #[test]
    fn logical_on_non_bool_is_error() {
        assert!(matches!(sema_error("1 and 2"), SemaError::Type(_)));
        assert!(matches!(sema_error("not 1"), SemaError::Type(_)));
    }

    #[test]
    fn bool_function_signature() {
        assert_eq!(
            root_type(indoc! {"
                fu(a: bool, b: bool): bool is a and b or a xor b
                fu(true, false)
            "}),
            PinpType::Bool
        );
    }

    // --- control flow --------------------------------------------------------------------

    #[test]
    fn if_expression_type_is_branch_join() {
        assert_eq!(root_type("1 if true else 2"), PinpType::Int);
        assert_eq!(root_type("1 if true else 2.0"), PinpType::Float); // join(Int, Float)
        assert_eq!(root_type("true if false else false"), PinpType::Bool);
    }

    #[test]
    fn condition_must_be_bool() {
        assert!(matches!(sema_error("1 if 5 else 2"), SemaError::Type(_)));
        assert!(matches!(
            sema_error(indoc! {"
                i = 0
                while i
                    i += 1
                i
            "}),
            SemaError::Type(_)
        ));
    }

    #[test]
    fn if_without_else_is_void_and_unusable_as_value() {
        assert!(matches!(
            sema_error(indoc! {"
                x = 0
                x = if true
                    1
            "}),
            SemaError::Type(_)
        ));
    }

    #[test]
    fn body_local_does_not_escape() {
        // `m` is introduced inside the if-body, so it is not in scope afterwards.
        assert!(matches!(
            sema_error(indoc! {"
                if true
                    m = 1
                m
            "}),
            SemaError::UnknownSymbol(_)
        ));
    }

    #[test]
    fn conditional_update_mutates_outer() {
        // `m` exists before the `if`; assigning it inside mutates that binding, so `m` stays in
        // scope (and Int) afterwards.
        assert_eq!(
            root_type(indoc! {"
                m = 5
                if true
                    m = 7
                m
            "}),
            PinpType::Int
        );
    }

    #[test]
    fn bare_loop_counter_type_checks() {
        assert_eq!(
            root_type(indoc! {"
                i = 0
                while i < 3
                    i += 1
                i
            "}),
            PinpType::Int
        );
    }

    // --- error cases ---------------------------------------------------------------------

    #[test]
    fn float_div_is_type_error() {
        assert!(matches!(sema_error("2.0 div 1"), SemaError::Type(_)));
    }

    #[test]
    fn unassigned_symbol_is_error() {
        assert!(matches!(sema_error("x + 1"), SemaError::UnknownSymbol(_)));
    }

    #[test]
    fn call_typechecks() {
        assert_eq!(
            root_type(indoc! {"
                sq(x: int): int is x*x
                sq(5)
            "}),
            PinpType::Int
        );
    }

    #[test]
    fn call_arg_promotes_int_to_float() {
        assert_eq!(
            root_type(indoc! {"
                f(x: float): float is x
                f(3)
            "}),
            PinpType::Float
        );
    }

    #[test]
    fn call_arity_mismatch_is_error() {
        assert!(matches!(
            sema_error(indoc! {"
                sq(x: int): int is x*x
                sq(1, 2)
            "}),
            SemaError::Type(_)
        ));
    }

    #[test]
    fn call_arg_type_mismatch_is_error() {
        assert!(matches!(
            sema_error(indoc! {"
                sq(x: int): int is x*x
                sq(1.5)
            "}),
            SemaError::Type(_)
        ));
    }

    #[test]
    fn call_before_definition_is_error() {
        assert!(matches!(
            sema_error(indoc! {"
                foo(1)
                foo(x: int): int is x
            "}),
            SemaError::UnknownSymbol(_)
        ));
    }

    #[test]
    fn bare_name_does_not_see_global() {
        assert!(matches!(
            sema_error(indoc! {"
                g = 10
                f(a: int): int is a + g
            "}),
            SemaError::UnknownSymbol(_)
        ));
    }

    #[test]
    fn compound_assign_local() {
        assert_eq!(
            root_type(indoc! {"
                f(a: int): int is
                    b = a
                    b += 1
                    b
                f(1)
            "}),
            PinpType::Int
        );
    }

    #[test]
    fn compound_div_breaks_int_place() {
        // `b` is Int; `b /= 2` yields Float, not assignable back to Int.
        assert!(matches!(
            sema_error(indoc! {"
                f(a: int): int is
                    b = a
                    b /= 2
                    b
            "}),
            SemaError::Type(_)
        ));
    }

    #[test]
    fn void_function_with_value_body_is_error() {
        assert!(matches!(
            sema_error(indoc! {"
                f(a: int) is
                    a + 1
            "}),
            SemaError::Type(_)
        ));
    }

    #[test]
    fn return_float_to_int_is_error() {
        assert!(matches!(
            sema_error("f(a: float): int is a"),
            SemaError::Type(_)
        ));
    }

    #[test]
    fn void_parameter_is_error() {
        // `void` is the no-return marker, not a value type, so it cannot be a parameter.
        assert!(matches!(
            sema_error("f(a: void): int is 1"),
            SemaError::Type(_)
        ));
    }

    #[test]
    fn global_access_and_compound_assign() {
        assert_eq!(
            root_type(indoc! {"
                g = 10
                bump(a: int): int is
                    ::g += 1
                    a + ::g
                bump(5)
            "}),
            PinpType::Int
        );
    }
}
