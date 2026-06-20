// SPDX-License-Identifier: MIT

use super::*;
use crate::parser::{BinOp, Block, ExprId, FuncDef, Place, RangeKind, Stmt, UnOp};

impl Analyzer<'_, '_> {
    // -------------------------------------------------------------------------
    // Functions and statements
    // -------------------------------------------------------------------------

    /// The source text of an interned symbol.
    fn name(&self, sym_id: SymId) -> &str {
        self.names[sym_id.value()]
    }

    pub(super) fn analyze_func(&mut self, func: &FuncDef) -> Result<(), SemaError> {
        let mut frame = FxHashMap::default();
        for param in &func.params {
            // `void` is the no-return marker, not a value type — rejecting it here is what keeps a
            // `Void` value from ever entering an expression.
            if param.param_type == PinpType::Void {
                return Err(SemaError::Type(format!(
                    "Function argument `{}` cannot be void.",
                    self.name(param.name)
                )));
            }
            frame.insert(param.name, param.param_type); // duplicate params already rejected by the parser
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
                params: func.params.iter().map(|param| param.param_type).collect(),
                return_type: func.return_type,
            },
        );
        Ok(())
    }

    pub(super) fn analyze_stmt(&mut self, stmt: &Stmt) -> Result<(), SemaError> {
        match stmt {
            Stmt::Expr(expr_id) => {
                self.analyze_expr(*expr_id)?;
                Ok(())
            }
            Stmt::Assign {
                target_lists,
                values,
            } => {
                // Type every value first (rejecting `Void`), then assign positionally to each target
                // group — arity (group length == value count) is guaranteed by the parser.
                let mut value_types = Vec::with_capacity(values.len());
                for value in values {
                    let value_type = self.analyze_expr(*value)?;
                    if value_type == PinpType::Void {
                        return Err(SemaError::Type("Cannot assign a void value.".into()));
                    }
                    value_types.push(value_type);
                }
                for group in target_lists {
                    for (place, value_type) in group.iter().zip(&value_types) {
                        self.assign_place(*place, *value_type)?;
                    }
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
            Stmt::For { var, range, body } => {
                let range_type = self.analyze_expr(*range)?;
                if range_type != PinpType::Range {
                    return Err(SemaError::Type(format!(
                        "`for` requires a range to iterate, got {range_type:?}."
                    )));
                }
                // Seed a body frame with the loop counter (`Int`, read-only) before analysing the
                // body; the inner block scope nests within it, so reads resolve outward to `var`.
                self.scopes.push(FxHashMap::default());
                self.scopes.last_mut().unwrap().insert(*var, PinpType::Int);
                self.loop_vars.push(*var);
                self.analyze_block(body)?;
                self.loop_vars.pop();
                self.scopes.pop();
                Ok(())
            }
        }
    }

    /// Checks one assignment target against the type of the value bound to it: a `::global` must
    /// already exist; a bare name mutates the nearest enclosing binding, or introduces a fresh
    /// non-escaping local (the 0006 scoping rules).
    fn assign_place(&mut self, place: Place, value_type: PinpType) -> Result<(), SemaError> {
        match place {
            Place::Global(sym_id) => match self.scopes[0].get(&sym_id).copied() {
                None => Err(SemaError::UnknownSymbol(format!(
                    "Unknown global `::{}`.",
                    self.name(sym_id)
                ))),
                Some(existing) => self.check_assignable(value_type, existing),
            },
            Place::Local(sym_id) => {
                if self.loop_vars.contains(&sym_id) {
                    return Err(SemaError::Type(format!(
                        "Cannot assign to loop variable `{}`.",
                        self.name(sym_id)
                    )));
                }
                match self.lookup_assign_target(sym_id) {
                    Some(existing) => self.check_assignable(value_type, existing),
                    None => {
                        self.scopes.last_mut().unwrap().insert(sym_id, value_type);
                        Ok(())
                    }
                }
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
            Some(expr_id) => Some(self.analyze_expr(expr_id)?),
            None => None,
        };
        self.scopes.pop();
        Ok(result_type)
    }

    /// A condition (`if`/`while`/`loop`) must be `Bool` — there is no truthiness.
    fn check_condition(&mut self, cond: ExprId) -> Result<(), SemaError> {
        let cond_type = self.analyze_expr(cond)?;
        if cond_type == PinpType::Bool {
            Ok(())
        } else {
            Err(SemaError::Type(format!(
                "Condition must be Bool, got {cond_type:?}."
            )))
        }
    }

    /// Finds the type of the nearest enclosing binding of `sym_id` (innermost first, down to the
    /// function base), or `None` if `sym_id` is bound nowhere in scope — meaning an assignment
    /// should introduce it as a new local.
    fn lookup_assign_target(&self, sym_id: SymId) -> Option<PinpType> {
        self.scopes[self.fn_base..]
            .iter()
            .rev()
            .find_map(|frame| frame.get(&sym_id).copied())
    }

    /// Errors unless `from` is assignable to `to` under the promotion lattice.
    fn check_assignable(&self, from: PinpType, to: PinpType) -> Result<(), SemaError> {
        if assignable(from, to) {
            Ok(())
        } else {
            Err(SemaError::Type(format!(
                "Assignment yields {from:?}, not assignable to {to:?}."
            )))
        }
    }

    // -------------------------------------------------------------------------
    // Expression typing
    // -------------------------------------------------------------------------

    /// Infers the type of `expr_id`, records it in the arena, and returns it.
    fn analyze_expr(&mut self, expr_id: ExprId) -> Result<PinpType, SemaError> {
        let inferred = match self.nodes[expr_id.value()].clone() {
            Node::Int(_) => PinpType::Int,
            Node::Float(_) => PinpType::Float,
            Node::Bool(_) => PinpType::Bool,
            Node::Var(sym_id) => self.lookup_local(sym_id)?,
            Node::Global(sym_id) => self.lookup_global(sym_id)?,
            Node::Unary { op, operand } => {
                let operand_type = self.analyze_expr(operand)?;
                self.unary_type(op, operand_type)?
            }
            Node::Bin { op, lhs, rhs } => {
                let left_type = self.analyze_expr(lhs)?;
                let right_type = self.analyze_expr(rhs)?;
                self.bin_type(op, left_type, right_type)?
            }
            Node::Call { callee, args } => self.call_type(callee, &args)?,
            Node::If { arms, else_block } => self.if_type(&arms, else_block.as_ref())?,
            Node::Range {
                start,
                stop,
                step,
                kind,
            } => self.range_type(start, stop, step, kind)?,
            Node::Membership { value, range } => self.membership_type(value, range)?,
        };
        self.types[expr_id.value()] = inferred;
        Ok(inferred)
    }

    /// Types an `if`: each condition must be `Bool`; the node's type is the join of every branch's
    /// result, but only when an `else` is present and every branch (arms and `else`) ends in an
    /// expression. Otherwise it is `Void` — usable as a statement, but not as a value.
    fn if_type(
        &mut self,
        arms: &[crate::parser::IfArm],
        else_block: Option<&Block>,
    ) -> Result<PinpType, SemaError> {
        let mut branch_types = Vec::with_capacity(arms.len());
        for arm in arms {
            self.check_condition(arm.cond)?;
            branch_types.push(self.analyze_block(&arm.body)?);
        }
        let Some(else_body) = else_block else {
            return Ok(PinpType::Void);
        };
        let else_type = self.analyze_block(else_body)?;

        // A missing branch result, or branches that do not share a common type, leave the `if`
        // valueless (`Void`).
        let mut joined = match else_type {
            Some(branch_type) => branch_type,
            None => return Ok(PinpType::Void),
        };
        for branch in branch_types {
            match branch.and_then(|branch_type| join(joined, branch_type)) {
                Some(wider) => joined = wider,
                None => return Ok(PinpType::Void),
            }
        }
        Ok(joined)
    }

    /// Types a range: each part (`start`, `stop`, and `step` if given) must be `Int`-like; the
    /// node's type is `Range`. When the parts are literals, the operator/direction is validated;
    /// variable parts are left to run time (an inconsistent one yields an empty range).
    fn range_type(
        &mut self,
        start: ExprId,
        stop: ExprId,
        step: Option<ExprId>,
        kind: RangeKind,
    ) -> Result<PinpType, SemaError> {
        let start_type = self.analyze_expr(start)?;
        self.require_int_part(start_type)?;
        let stop_type = self.analyze_expr(stop)?;
        self.require_int_part(stop_type)?;
        if let Some(step_id) = step {
            let step_type = self.analyze_expr(step_id)?;
            self.require_int_part(step_type)?;
            // A literal zero step is invalid whatever the bounds are (it never advances); a variable
            // step that is zero at run time yields an empty range instead.
            if self.int_literal(step_id) == Some(0) {
                return Err(SemaError::Type("Range step cannot be zero.".into()));
            }
        }
        self.validate_literal_range(start, stop, step, kind)?;
        Ok(PinpType::Range)
    }

    /// A range bound or step must be `Int`-like (`Bool`/`Int`).
    fn require_int_part(&self, part_type: PinpType) -> Result<(), SemaError> {
        if int_like(part_type) {
            Ok(())
        } else {
            Err(SemaError::Type(format!(
                "Range bounds and step must be Int, got {part_type:?}."
            )))
        }
    }

    /// The literal value of `expr_id` if it is an integer literal — the hook for compile-time range
    /// validation, which only applies when the bounds (and step) are constants.
    fn int_literal(&self, expr_id: ExprId) -> Option<i64> {
        match self.nodes[expr_id.value()] {
            Node::Int(value) => Some(value),
            // A negative literal (e.g. a `:-2` step) parses as unary minus over an integer literal.
            Node::Unary {
                op: UnOp::Neg,
                operand,
            } => match self.nodes[operand.value()] {
                Node::Int(value) => Some(-value),
                _ => None,
            },
            _ => None,
        }
    }

    /// Rejects an ill-formed range whose bounds are literals: an inclusive range whose step opposes
    /// its direction, or an exclusive operator pointing the wrong way. (A zero step is rejected
    /// earlier, in `range_type`, since it is invalid regardless of the bounds.)
    fn validate_literal_range(
        &self,
        start: ExprId,
        stop: ExprId,
        step: Option<ExprId>,
        kind: RangeKind,
    ) -> Result<(), SemaError> {
        let (Some(start_value), Some(stop_value)) =
            (self.int_literal(start), self.int_literal(stop))
        else {
            return Ok(()); // a variable bound is a run-time concern
        };
        let step_value = step.and_then(|step_id| self.int_literal(step_id));
        match kind {
            RangeKind::Inclusive => {
                if let Some(step) = step_value {
                    if start_value < stop_value && step < 0 {
                        return Err(SemaError::Type(
                            "The range ascends, but the step is negative.".into(),
                        ));
                    }
                    if start_value > stop_value && step > 0 {
                        return Err(SemaError::Type(
                            "The range descends, but the step is positive.".into(),
                        ));
                    }
                }
            }
            RangeKind::UpExclusive => {
                if start_value >= stop_value {
                    return Err(SemaError::Type("A descending range must use ..>.".into()));
                }
            }
            RangeKind::DownExclusive => {
                if start_value <= stop_value {
                    return Err(SemaError::Type("An ascending range must use ..<.".into()));
                }
            }
        }
        Ok(())
    }

    /// Types a membership test: the tested value must be `Int`-like and the right operand a range;
    /// the result is `Bool`.
    fn membership_type(&mut self, value: ExprId, range: ExprId) -> Result<PinpType, SemaError> {
        let value_type = self.analyze_expr(value)?;
        if !int_like(value_type) {
            return Err(SemaError::Type(format!(
                "The value tested with `in` must be Int, got {value_type:?}."
            )));
        }
        let range_type = self.analyze_expr(range)?;
        if range_type != PinpType::Range {
            return Err(SemaError::Type(format!(
                "`in` requires a range on the right, got {range_type:?}."
            )));
        }
        Ok(PinpType::Bool)
    }

    /// The type of a bare name, resolved outward to the function base.
    fn lookup_local(&self, sym_id: SymId) -> Result<PinpType, SemaError> {
        // Reading and assigning resolve a bare name the same way — outward to the function base —
        // so both share `lookup_assign_target`.
        self.lookup_assign_target(sym_id).ok_or_else(|| {
            SemaError::UnknownSymbol(format!("Unknown symbol `{}`.", self.name(sym_id)))
        })
    }

    /// The type of a `::global`, looked up in the global frame.
    fn lookup_global(&self, sym_id: SymId) -> Result<PinpType, SemaError> {
        self.scopes[0].get(&sym_id).copied().ok_or_else(|| {
            SemaError::UnknownSymbol(format!("Unknown global `::{}`.", self.name(sym_id)))
        })
    }

    /// The result type of a unary operator applied to `operand_type`.
    fn unary_type(&self, op: UnOp, operand_type: PinpType) -> Result<PinpType, SemaError> {
        match op {
            // Arithmetic negation: `Bool` promotes to `Int` (like any arithmetic use of a bool);
            // `Void`/`Range` are not numeric and are rejected.
            UnOp::Neg => match operand_type {
                PinpType::Bool | PinpType::Int => Ok(PinpType::Int),
                PinpType::Float => Ok(PinpType::Float),
                PinpType::Void | PinpType::Range => Err(SemaError::Type(format!(
                    "Unary minus requires a numeric operand, got {operand_type:?}."
                ))),
            },
            UnOp::Not => {
                if operand_type == PinpType::Bool {
                    Ok(PinpType::Bool)
                } else {
                    Err(SemaError::Type(format!(
                        "`not` requires a Bool operand, got {operand_type:?}."
                    )))
                }
            }
        }
    }

    /// The result type of a binary operator on `left_type`/`right_type` (logicals, comparisons, and
    /// arithmetic, with `Bool`/`Int` promotion).
    fn bin_type(
        &self,
        op: BinOp,
        left_type: PinpType,
        right_type: PinpType,
    ) -> Result<PinpType, SemaError> {
        use BinOp::*;
        use PinpType::*;
        // Logicals take and yield Bool only — no truthiness.
        if matches!(op, And | Or | Xor) {
            return if left_type == Bool && right_type == Bool {
                Ok(Bool)
            } else {
                Err(SemaError::Type(format!(
                    "`{op:?}` requires Bool operands, got {left_type:?} and {right_type:?}."
                )))
            };
        }
        // Arithmetic and comparison both require scalar operands — never `Void` or a `Range`.
        if !numeric(left_type) || !numeric(right_type) {
            return Err(SemaError::Type(format!(
                "`{op:?}` requires numeric operands, got {left_type:?} and {right_type:?}."
            )));
        }
        Ok(match op {
            Eq | Ne | Lt | Gt | Le | Ge => Bool,
            Add | Sub | Mul | Pow => {
                if int_like(left_type) && int_like(right_type) {
                    Int
                } else {
                    Float
                }
            }
            Div => Float,
            IntDiv | Mod => {
                if int_like(left_type) && int_like(right_type) {
                    Int
                } else {
                    return Err(SemaError::Type(format!("{op:?} requires Int operands.")));
                }
            }
            And | Or | Xor => unreachable!("handled above"),
        })
    }

    /// Types a call: the callee must be defined, with matching arity and assignable arguments.
    fn call_type(&mut self, callee: SymId, args: &[ExprId]) -> Result<PinpType, SemaError> {
        let signature = self.funcs.get(&callee).cloned().ok_or_else(|| {
            SemaError::UnknownSymbol(format!(
                "Call to undefined function `{}`.",
                self.name(callee)
            ))
        })?;
        if args.len() != signature.params.len() {
            return Err(SemaError::Type(format!(
                "Function `{}` expects {} argument(s), got {}.",
                self.name(callee),
                signature.params.len(),
                args.len()
            )));
        }
        for (index, (&arg, &param_type)) in args.iter().zip(signature.params.iter()).enumerate() {
            let arg_type = self.analyze_expr(arg)?;
            if !assignable(arg_type, param_type) {
                return Err(SemaError::Type(format!(
                    "Argument {} of `{}`: {arg_type:?} not assignable to {param_type:?}.",
                    index + 1,
                    self.name(callee)
                )));
            }
        }
        Ok(signature.return_type)
    }
}
