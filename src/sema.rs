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

use crate::parser::{Ast, BinOp, ExprId, FuncDef, Node, PinpType, Place, Stmt, SymId, TopLevel};

/// A semantic failure. pinp is fail-fast: the first error stops the pass.
#[derive(Debug, Clone, PartialEq)]
pub enum SemaError {
    /// A name used but not bound: unknown variable, global, or function.
    UnknownSymbol(String),
    /// A type error: a mismatch, a bad promotion, or an operation on `Void`.
    Type(String),
}

/// `from` is assignable to `to` if identical, or via the one allowed promotion `Int -> Float`.
fn assignable(from: PinpType, to: PinpType) -> bool {
    from == to || (from == PinpType::Int && to == PinpType::Float)
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
    funcs: FxHashMap<SymId, Signature>,
}

impl Analyzer<'_, '_> {
    fn name(&self, s: SymId) -> &str {
        self.names[s.0 as usize]
    }

    fn analyze_func(&mut self, func: &FuncDef) -> Result<(), SemaError> {
        let mut frame = FxHashMap::default();
        for p in &func.params {
            frame.insert(p.name, p.param_type); // duplicate params already rejected by the parser
        }
        self.scopes.push(frame);
        for stmt in &func.body.stmts {
            self.analyze_stmt(stmt)?;
        }
        let result_type = self.analyze_expr(func.body.result)?;
        self.scopes.pop();

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
                    Place::Local(s) => {
                        let frame = self.scopes.last().unwrap();
                        match frame.get(&s).copied() {
                            Some(existing) => self.check_assignable(rhs_type, existing)?,
                            None => {
                                self.scopes.last_mut().unwrap().insert(s, rhs_type);
                            }
                        }
                    }
                }
                Ok(())
            }
        }
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
            Node::Var(s) => self.lookup_local(s)?,
            Node::Global(s) => self.lookup_global(s)?,
            Node::Unary { operand, .. } => {
                let t = self.analyze_expr(operand)?;
                if t == PinpType::Void {
                    return Err(SemaError::Type("Unary minus on a void value.".into()));
                }
                t
            }
            Node::Bin { op, lhs, rhs } => {
                let lt = self.analyze_expr(lhs)?;
                let rt = self.analyze_expr(rhs)?;
                self.bin_type(op, lt, rt)?
            }
            Node::Call { callee, args } => self.call_type(callee, &args)?,
        };
        self.types[e.0 as usize] = ty;
        Ok(ty)
    }

    fn lookup_local(&self, s: SymId) -> Result<PinpType, SemaError> {
        self.scopes
            .last()
            .unwrap()
            .get(&s)
            .copied()
            .ok_or_else(|| SemaError::UnknownSymbol(format!("Unknown symbol `{}`.", self.name(s))))
    }

    fn lookup_global(&self, s: SymId) -> Result<PinpType, SemaError> {
        self.scopes[0]
            .get(&s)
            .copied()
            .ok_or_else(|| SemaError::UnknownSymbol(format!("Unknown global `::{}`.", self.name(s))))
    }

    fn bin_type(&self, op: BinOp, lt: PinpType, rt: PinpType) -> Result<PinpType, SemaError> {
        use PinpType::*;
        if lt == Void || rt == Void {
            return Err(SemaError::Type("Arithmetic on a void value.".into()));
        }
        let both_int = lt == Int && rt == Int;
        Ok(match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Pow => {
                if both_int {
                    Int
                } else {
                    Float
                }
            }
            BinOp::Div => Float,
            BinOp::IntDiv | BinOp::Mod => {
                if !both_int {
                    return Err(SemaError::Type(format!("{op:?} requires Int operands.")));
                }
                Int
            }
        })
    }

    fn call_type(&mut self, callee: SymId, args: &[ExprId]) -> Result<PinpType, SemaError> {
        let sig = self.funcs.get(&callee).cloned().ok_or_else(|| {
            SemaError::UnknownSymbol(format!("Call to undefined function `{}`.", self.name(callee)))
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

    // A `void` parameter is the only way to bring a `Void` value into an expression, so these
    // exercise the otherwise-guarded `Void`-operand checks.

    #[test]
    fn arithmetic_on_void_is_error() {
        assert!(matches!(
            sema_error("f(a: void): int is a + 1"),
            SemaError::Type(_)
        ));
    }

    #[test]
    fn unary_minus_on_void_is_error() {
        assert!(matches!(
            sema_error("f(a: void): int is -a"),
            SemaError::Type(_)
        ));
    }

    #[test]
    fn assign_void_is_error() {
        assert!(matches!(
            sema_error(indoc! {"
                f(a: void): int is
                    x = a
                    1
            "}),
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
