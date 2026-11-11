// SPDX-License-Identifier: MIT

use super::*;
use crate::parser::{
    ArrayElementType, BinOp, Block, BuiltinMember, ExprId, FStrSegment, FuncDef, Place, RangeKind,
    Stmt, UnOp,
};

/// Types a binary operator where at least one operand is `Str`. Only concatenation (`+`) and the six
/// comparisons are defined; `+` concatenates and implicitly `str()`-wraps a scalar right operand,
/// while a comparison requires both operands to already be `Str`.
fn string_bin_type(op: BinOp, left: PinpType, right: PinpType) -> Result<PinpType, SemaErrorKind> {
    use BinOp::*;
    use PinpType::*;
    match op {
        Eq | Ne | Lt | Gt | Le | Ge => {
            if left == Str && right == Str {
                Ok(Bool)
            } else {
                Err(SemaErrorKind::Type(format!(
                    "Cannot compare str with {:?}.",
                    if left == Str { right } else { left }
                )))
            }
        }
        // `str + x`: the left operand drives concatenation and the right is implicitly `str()`-wrapped,
        // so it must itself be a scalar or a `str`. `x + str` with a non-`str` left is an error.
        Add if left == Str => {
            if right == Str || numeric(right) {
                Ok(Str)
            } else {
                Err(SemaErrorKind::Type(format!(
                    "Cannot concatenate {right:?} to a str."
                )))
            }
        }
        Add => Err(SemaErrorKind::Type(
            "Left operand of `+` must be a str when the right is.".into(),
        )),
        // TODO(KI-0002): `{op:?}` prints the Rust variant name (`Sub`), not the operator the
        // reader wrote (`-`).
        _ => Err(SemaErrorKind::Type(format!(
            "`{op:?}` is not defined for str."
        ))),
    }
}

impl Analyzer<'_, '_> {
    // -------------------------------------------------------------------------
    // Functions and statements
    // -------------------------------------------------------------------------

    /// The source text of an interned symbol.
    fn name(&self, sym_id: SymId) -> &str {
        self.names[sym_id.value()]
    }

    pub(super) fn analyze_func(&mut self, func: &FuncDef) -> Result<(), SemaErrorKind> {
        // Built-in names are reserved; a user definition with same name is an error.
        // NOTE: when the list of buil-ins grows, we will need to consult internal
        //       list of built-in headers.
        if matches!(self.name(func.name), "identity" | "str" | "meminfo") {
            let kind = SemaErrorKind::Type(format!(
                "Cannot define a function named `{}`: conflicts with a built-in.",
                self.name(func.name)
            ));
            return Err(self.error_at(func.name_span, kind));
        }
        let mut frame = FxHashMap::default();
        for param in &func.params {
            // `void` is the no-return marker, not a value type — rejecting it here is what keeps a
            // `Void` value from ever entering an expression.
            if param.param_type == PinpType::Void {
                let kind = SemaErrorKind::Type(format!(
                    "Function argument `{}` cannot be void.",
                    self.name(param.name)
                ));
                return Err(self.error_at(func.name_span, kind));
            }
            // `str` parameters are deferred this iteration (their cross-call freeing model is out of
            // scope). The annotation parses; rejecting it here keeps `Str` from reaching codegen.
            if param.param_type == PinpType::Str {
                let kind = SemaErrorKind::Type("String parameters are not yet supported.".into());
                return Err(self.error_at(func.name_span, kind));
            }
            frame.insert(param.name, param.param_type); // duplicate params already rejected by the parser
        }
        let old_base = self.fn_base;
        self.scopes.push(frame);
        self.fn_base = self.scopes.len() - 1;
        self.current_function = Some((func.name, func.return_type));
        let mut diverges = false;
        for stmt in &func.body.stmts {
            diverges |= self.analyze_stmt(stmt)?;
        }
        let result = match func.body.result {
            Some(result) => Some((result, self.analyze_expr(result)?)),
            None => None,
        };
        self.current_function = None;
        self.scopes.pop();
        self.fn_base = old_base;

        let diverges = diverges || matches!(result, Some((_, PinpType::Never)));
        match result {
            _ if diverges => {}
            Some((result, result_type)) if result_type != PinpType::Void => {
                if !assignable(result_type, func.return_type) {
                    let kind = SemaErrorKind::Type(format!(
                        "Function `{}` body yields {result_type:?} but is declared {:?}.",
                        self.name(func.name),
                        func.return_type
                    ));
                    return Err(self.error_at_expr(result, kind));
                }
            }
            _ if func.return_type != PinpType::Void => {
                let kind = SemaErrorKind::Coded(
                    DiagnosticCode::MissingReturn,
                    format!(
                        "Not all paths return a value in function `{}()`.",
                        self.name(func.name)
                    ),
                );
                return Err(self.error_at(func.name_span, kind));
            }
            _ => {}
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

    /// Checks one statement, returning whether it *diverges* — control never continues past it:
    /// a `return`, or an `if` every branch of which returns.
    pub(super) fn analyze_stmt(&mut self, stmt: &Stmt) -> Result<bool, SemaErrorKind> {
        match stmt {
            Stmt::Expr(expr_id) => Ok(self.analyze_expr(*expr_id)? == PinpType::Never),
            Stmt::Return { value, span } => {
                self.analyze_return(*value, *span)?;
                Ok(true)
            }
            Stmt::Assign {
                target_lists,
                target_spans,
                values,
            } => {
                // Type every value first (rejecting `Void`), then assign positionally to each target
                // group — arity (group length == value count) is guaranteed by the parser.
                let mut value_types = Vec::with_capacity(values.len());
                for value in values {
                    let value_type = self.analyze_expr(*value)?;
                    if value_type == PinpType::Void {
                        let kind = SemaErrorKind::Type("Cannot assign a void value.".into());
                        return Err(self.error_at_expr(*value, kind));
                    }
                    if value_type == PinpType::Never {
                        let kind = SemaErrorKind::Coded(
                            DiagnosticCode::NeverValue,
                            "Expression never produces a value.".into(),
                        );
                        return Err(self.error_at_expr(*value, kind));
                    }
                    value_types.push(value_type);
                }
                for (group, spans) in target_lists.iter().zip(target_spans) {
                    for ((place, target_span), (value_type, value)) in
                        group.iter().zip(spans).zip(value_types.iter().zip(values))
                    {
                        self.assign_place(*place, *target_span, *value, *value_type)?;
                    }
                }
                Ok(false)
            }
            // A loop never diverges, even when its body does: the body may not run at all.
            Stmt::While { cond, body } => {
                self.check_condition(*cond)?;
                self.analyze_block(body)?;
                Ok(false)
            }
            Stmt::Loop { body, cond, .. } => {
                self.analyze_block(body)?;
                self.check_condition(*cond)?;
                Ok(false)
            }
            Stmt::For {
                var, source, body, ..
            } => {
                let source_type = self.analyze_expr(*source)?;
                // Range → Int counter; Array/Matrix → element type; anything else → error.
                let var_type = match source_type {
                    PinpType::Range => PinpType::Int,
                    PinpType::Array(elem_type, _) | PinpType::Matrix(elem_type, _, _) => {
                        PinpType::from(elem_type)
                    }
                    other => {
                        let kind = SemaErrorKind::Type(format!("Cannot iterate over {other:?}."));
                        return Err(self.error_at_expr(*source, kind));
                    }
                };
                // Seed a body frame with the loop variable (read-only) before analysing the body;
                // the inner block scope nests within it, so reads resolve outward to `var`.
                self.scopes.push(FxHashMap::default());
                self.scopes.last_mut().unwrap().insert(*var, var_type);
                self.loop_vars.push(*var);
                self.analyze_block(body)?;
                self.loop_vars.pop();
                self.scopes.pop();
                Ok(false)
            }
            Stmt::ForArray {
                binders,
                source,
                body,
                ..
            } => {
                let source_type = self.analyze_expr(*source)?;
                let mut frame = FxHashMap::default();
                match (binders.len(), source_type) {
                    (2, PinpType::Array(elem_type, _)) => {
                        frame.insert(binders[0], PinpType::Int);
                        frame.insert(binders[1], PinpType::from(elem_type));
                    }
                    (3, PinpType::Matrix(elem_type, _, _)) => {
                        frame.insert(binders[0], PinpType::Int);
                        frame.insert(binders[1], PinpType::Int);
                        frame.insert(binders[2], PinpType::from(elem_type));
                    }
                    _ => {
                        let kind =
                            SemaErrorKind::Type("Binder count does not match array rank.".into());
                        return Err(self.error_at_expr(*source, kind));
                    }
                }
                // Binders share one scope frame, so a repeated name would silently keep only the
                // last position's value. `_` is the discard binder and may repeat freely.
                for (position, binder) in binders.iter().enumerate() {
                    if binders[..position].contains(binder) && self.name(*binder) != "_" {
                        let kind = SemaErrorKind::Type(format!(
                            "Duplicate binder `{}`.",
                            self.name(*binder)
                        ));
                        return Err(self.error_at_expr(*source, kind));
                    }
                }
                self.scopes.push(frame);
                for binder in binders.iter() {
                    self.loop_vars.push(*binder);
                }
                self.analyze_block(body)?;
                for _ in binders.iter() {
                    self.loop_vars.pop();
                }
                self.scopes.pop();
                Ok(false)
            }
            Stmt::IndexedAssign {
                target,
                target_span,
                index,
                value,
                compound_op,
            } => {
                self.indexed_assign_check(*target, *target_span, *index, *value, *compound_op)?;
                Ok(false)
            }
            Stmt::IndexedAssign2D {
                target,
                target_span,
                row,
                col,
                value,
                compound_op,
            } => {
                self.indexed_assign2d_check(
                    *target,
                    *target_span,
                    *row,
                    *col,
                    *value,
                    *compound_op,
                )?;
                Ok(false)
            }
        }
    }

    /// Checks a `return` against the enclosing function: it must be in one, and carry a value
    /// exactly when the function has a return type, of a type assignable to it.
    fn analyze_return(&mut self, value: Option<ExprId>, span: Span) -> Result<(), SemaErrorKind> {
        let Some((name, return_type)) = self.current_function else {
            let kind = SemaErrorKind::Coded(
                DiagnosticCode::ReturnOutsideFunction,
                "Return outside a function.".into(),
            );
            return Err(self.error_at(span, kind));
        };
        let Some(value) = value else {
            if return_type == PinpType::Void {
                return Ok(());
            }
            let kind = SemaErrorKind::Coded(
                DiagnosticCode::MissingReturnValue,
                format!(
                    "Function `{}` must return a value of type {return_type:?}.",
                    self.name(name)
                ),
            );
            return Err(self.error_at(span, kind));
        };
        let value_type = self.analyze_expr(value)?;
        if return_type == PinpType::Void {
            let kind = SemaErrorKind::Coded(
                DiagnosticCode::VoidFunctionReturnsValue,
                format!(
                    "Function `{}` is void and cannot return a value.",
                    self.name(name)
                ),
            );
            return Err(self.error_at_expr(value, kind));
        }
        if !assignable(value_type, return_type) {
            let kind = SemaErrorKind::Coded(
                DiagnosticCode::ReturnTypeMismatch,
                format!(
                    "Function `{}` returns {value_type:?} but is declared {return_type:?}.",
                    self.name(name)
                ),
            );
            return Err(self.error_at_expr(value, kind));
        }
        Ok(())
    }

    /// Checks one assignment target against the type of the value bound to it: a `::global` must
    /// already exist; a bare name mutates the nearest enclosing binding, or introduces a fresh
    /// non-escaping local (the 0006 scoping rules).
    ///
    /// The two positions are both needed and are not interchangeable: a name that is not bound or
    /// may not be written is a complaint about the *target*, while a type mismatch is a complaint
    /// about the *value* — the target is fine, the thing being stored into it is not.
    fn assign_place(
        &mut self,
        place: Place,
        target_span: Span,
        value: ExprId,
        value_type: PinpType,
    ) -> Result<(), SemaErrorKind> {
        match place {
            Place::Global(sym_id) => match self.scopes[0].get(&sym_id).copied() {
                None => {
                    let kind = SemaErrorKind::UnknownSymbol(format!(
                        "Unknown global `::{}`.",
                        self.name(sym_id)
                    ));
                    Err(self.error_at(target_span, kind))
                }
                Some(existing) => self
                    .check_assignable(value_type, existing)
                    .map_err(|kind| self.error_at_expr(value, kind)),
            },
            Place::Local(sym_id) => {
                if self.loop_vars.contains(&sym_id) {
                    let kind = SemaErrorKind::Type(format!(
                        "Cannot assign to loop variable `{}`.",
                        self.name(sym_id)
                    ));
                    return Err(self.error_at(target_span, kind));
                }
                match self.lookup_assign_target(sym_id) {
                    Some(existing) => self
                        .check_assignable(value_type, existing)
                        .map_err(|kind| self.error_at_expr(value, kind)),
                    None => {
                        self.scopes.last_mut().unwrap().insert(sym_id, value_type);
                        Ok(())
                    }
                }
            }
        }
    }

    /// Analyses a control-flow body in its own scope, returning its type: `Never` when a statement
    /// in it diverges, else the type of its trailing result expression, or `Void` when it ends in a
    /// statement. The scope is popped on the way out, so a name first assigned here does not
    /// escape.
    fn analyze_block(&mut self, block: &Block) -> Result<PinpType, SemaErrorKind> {
        self.scopes.push(FxHashMap::default());
        let mut diverges = false;
        for stmt in &block.stmts {
            diverges |= self.analyze_stmt(stmt)?;
        }
        let result_type = match block.result {
            Some(expr_id) => self.analyze_expr(expr_id)?,
            None => PinpType::Void,
        };
        self.scopes.pop();
        Ok(match diverges {
            true => PinpType::Never,
            false => result_type,
        })
    }

    /// A condition (`if`/`while`/`loop`) must be `Bool` — there is no truthiness.
    fn check_condition(&mut self, cond: ExprId) -> Result<(), SemaErrorKind> {
        let cond_type = self.analyze_expr(cond)?;
        if cond_type == PinpType::Bool {
            Ok(())
        } else {
            let kind = SemaErrorKind::Type(format!("Condition must be Bool, got {cond_type:?}."));
            Err(self.error_at_expr(cond, kind))
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
    fn check_assignable(&self, from: PinpType, to: PinpType) -> Result<(), SemaErrorKind> {
        if assignable(from, to) {
            Ok(())
        } else {
            Err(SemaErrorKind::Type(format!(
                "Assignment yields {from:?}, not assignable to {to:?}."
            )))
        }
    }

    // -------------------------------------------------------------------------
    // Expression typing
    // -------------------------------------------------------------------------

    /// Infers the type of `expr_id`, records it in the arena, and returns it.
    ///
    /// Also maintains `current_span`, which positions whatever error the walk ends on. The
    /// caller's value is restored **on success only**: once a child has been analysed the position
    /// is back on the parent, so a rule that fails after inspecting its operands — a type mismatch,
    /// say — points at the whole expression. On failure the innermost node's position stays put and
    /// propagates, because that is the node that actually broke a rule.
    fn analyze_expr(&mut self, expr_id: ExprId) -> Result<PinpType, SemaErrorKind> {
        let caller_span = self.current_span;
        self.current_span = self.spans[expr_id.value()];
        let result = self.infer_node_type(expr_id);
        if result.is_ok() {
            self.current_span = caller_span;
        }
        result
    }

    /// Positions `kind` at `span` — how a *statement-level* rule says what it blames.
    ///
    /// Expression rules get their position for free from `analyze_expr`, but a statement rule runs
    /// after its operands have been typed, and each of those restored the caller's position on the
    /// way out. What is left is the position of whatever enclosed the statement, which for a
    /// statement is nothing at all. So these rules name their subject explicitly: the value for an
    /// assignment mismatch, the index for a bad subscript, the target for a name that is not bound.
    fn error_at(&mut self, span: Span, kind: SemaErrorKind) -> SemaErrorKind {
        self.current_span = span;
        kind
    }

    /// [`error_at`](Self::error_at) for the common case of blaming an expression.
    fn error_at_expr(&mut self, expr_id: ExprId, kind: SemaErrorKind) -> SemaErrorKind {
        self.error_at(self.spans[expr_id.value()], kind)
    }

    /// The typing rules themselves, split out so `analyze_expr` can wrap every one of them in the
    /// same position bookkeeping.
    fn infer_node_type(&mut self, expr_id: ExprId) -> Result<PinpType, SemaErrorKind> {
        let inferred = match self.nodes[expr_id.value()].clone() {
            Node::Int(_) => PinpType::Int,
            Node::Float(_) => PinpType::Float,
            Node::Bool(_) => PinpType::Bool,
            Node::Str(_) => PinpType::Str,
            Node::FStr { segments } => {
                // Every interpolation hole must name a binding that stringifies — a scalar or another
                // `str`. An aggregate/`Void`/`Range` hole has no defined rendering and is an error.
                for segment in &segments {
                    if let FStrSegment::Interp(place) = segment {
                        let place_type = self.place_type(*place)?;
                        if !(numeric(place_type) || place_type == PinpType::Str) {
                            return Err(SemaErrorKind::Type(format!(
                                "Cannot interpolate {place_type:?} into an f-string."
                            )));
                        }
                    }
                }
                PinpType::Str
            }
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
            Node::ArrayLiteral { elements } => self.array_literal_type(&elements)?,
            Node::Index { array, index } => self.index_type(array, index)?,
            Node::Member { object, member } => self.member_type(expr_id, object, member)?,
            Node::Comprehension {
                element,
                var,
                var_type,
                source,
                ..
            } => self.comprehension_type(element, var, var_type, source)?,
            Node::MatrixLiteral { rows } => self.matrix_literal_type(&rows)?,
            Node::Index2D { matrix, row, col } => self.matrix_index2d_type(matrix, row, col)?,
            Node::FullExtent => {
                return Err(SemaErrorKind::Type(
                    "`:` is only valid inside a 2D index expression.".into(),
                ));
            }
        };
        self.types[expr_id.value()] = inferred;
        Ok(inferred)
    }

    /// Types an `if`: each condition must be `Bool`; the node's type is the join of every branch's
    /// type, but only when an `else` is present. Without one, or when the branches share no type,
    /// it is `Void` — usable as a statement, but not as a value. Branches that return do not take
    /// part, so an `if` all of whose branches return is `Never`.
    fn if_type(
        &mut self,
        arms: &[crate::parser::IfArm],
        else_block: Option<&Block>,
    ) -> Result<PinpType, SemaErrorKind> {
        let mut branch_types = Vec::with_capacity(arms.len() + 1);
        for arm in arms {
            self.check_condition(arm.cond)?;
            branch_types.push(self.analyze_block(&arm.body)?);
        }
        let Some(else_body) = else_block else {
            return Ok(PinpType::Void);
        };
        branch_types.push(self.analyze_block(else_body)?);

        let mut joined = PinpType::Never;
        for branch_type in branch_types {
            match join(joined, branch_type) {
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
    ) -> Result<PinpType, SemaErrorKind> {
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
                return Err(SemaErrorKind::Type("Range step cannot be zero.".into()));
            }
        }
        self.validate_literal_range(start, stop, step, kind)?;
        Ok(PinpType::Range)
    }

    /// A range bound or step must be `Int`-like (`Bool`/`Int`).
    fn require_int_part(&self, part_type: PinpType) -> Result<(), SemaErrorKind> {
        if int_like(part_type) {
            Ok(())
        } else {
            Err(SemaErrorKind::Type(format!(
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

    /// Compile-time bounds check for a scalar literal index. Non-literal indices pass through;
    /// the runtime normalization and check in codegen handles those.
    fn check_literal_index(&self, index_id: ExprId, len: usize) -> Result<(), SemaErrorKind> {
        if let Some(val) = self.int_literal(index_id) {
            let effective = if val < 0 { val + len as i64 } else { val };
            if effective < 0 || effective >= len as i64 {
                return Err(SemaErrorKind::Type("Array index out of bounds.".into()));
            }
        }
        Ok(())
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
    ) -> Result<(), SemaErrorKind> {
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
                        return Err(SemaErrorKind::Type(
                            "The range ascends, but the step is negative.".into(),
                        ));
                    }
                    if start_value > stop_value && step > 0 {
                        return Err(SemaErrorKind::Type(
                            "The range descends, but the step is positive.".into(),
                        ));
                    }
                }
            }
            RangeKind::UpExclusive => {
                // `start == stop` is an empty range (zero iterations), not a direction error.
                if start_value > stop_value {
                    return Err(SemaErrorKind::Type(
                        "A descending range must use ..>.".into(),
                    ));
                }
            }
            RangeKind::DownExclusive => {
                if start_value < stop_value {
                    return Err(SemaErrorKind::Type(
                        "An ascending range must use ..<.".into(),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Types a membership test: the tested value must be `Int`-like and the right operand a range;
    /// the result is `Bool`.
    fn membership_type(&mut self, value: ExprId, range: ExprId) -> Result<PinpType, SemaErrorKind> {
        let value_type = self.analyze_expr(value)?;
        if !int_like(value_type) {
            return Err(SemaErrorKind::Type(format!(
                "The value tested with `in` must be Int, got {value_type:?}."
            )));
        }
        let range_type = self.analyze_expr(range)?;
        if range_type != PinpType::Range {
            return Err(SemaErrorKind::Type(format!(
                "`in` requires a range on the right, got {range_type:?}."
            )));
        }
        Ok(PinpType::Bool)
    }

    /// Types an array literal: all elements are analysed and must share a common type.
    /// A single `Range` element is range-init syntax (`[start..stop[:step]]`); sema routes it
    /// to `array_from_range_type` rather than treating the range as a scalar element.
    fn array_literal_type(&mut self, elements: &[ExprId]) -> Result<PinpType, SemaErrorKind> {
        // The parser already rejects `[]`; this guard is a safety net.
        if elements.is_empty() {
            return Err(SemaErrorKind::Type("Array literal cannot be empty.".into()));
        }
        // Range-init: `[literal-range]` — the sole element is a Range node, not a scalar.
        if elements.len() == 1 && matches!(self.nodes[elements[0].value()], Node::Range { .. }) {
            return self.array_from_range_type(elements[0]);
        }
        let mut common = self.analyze_expr(elements[0])?;
        for &elem_id in &elements[1..] {
            let elem = self.analyze_expr(elem_id)?;
            common = join(common, elem).ok_or_else(|| {
                SemaErrorKind::Type("Array elements have incompatible types.".into())
            })?;
        }
        Ok(PinpType::Array(
            self.to_array_element_type(common)?,
            elements.len(),
        ))
    }

    /// Types a matrix literal `[r0_e0, …; r1_e0, …; …]`. All rows must have the same element
    /// count; all elements must share a common type under the promotion lattice.
    fn matrix_literal_type(&mut self, rows: &[Vec<ExprId>]) -> Result<PinpType, SemaErrorKind> {
        let col_count = rows[0].len();
        let mut common: Option<PinpType> = None;
        for row in rows {
            if row.len() != col_count {
                return Err(SemaErrorKind::Type(
                    "All matrix rows must have the same number of columns.".into(),
                ));
            }
            for &elem_id in row {
                let elem_type = self.analyze_expr(elem_id)?;
                common = Some(match common {
                    None => elem_type,
                    Some(previous) => join(previous, elem_type).ok_or_else(|| {
                        SemaErrorKind::Type("Inconsistent element types in matrix literal.".into())
                    })?,
                });
            }
        }
        let common = common.expect("parser ensures matrix has at least one element");
        Ok(PinpType::Matrix(
            self.to_array_element_type(common)?,
            rows.len(),
            col_count,
        ))
    }

    /// Types `matrix[row_sel, col_sel]`. Classifies each selector as a scalar index or a
    /// compile-time-checked slice, then derives the result type from the four-case table.
    fn matrix_index2d_type(
        &mut self,
        matrix: ExprId,
        row: ExprId,
        col: ExprId,
    ) -> Result<PinpType, SemaErrorKind> {
        let matrix_type = self.analyze_expr(matrix)?;
        let PinpType::Matrix(elem_type, row_count, col_count) = matrix_type else {
            return Err(SemaErrorKind::Type(
                "2D index target is not a matrix.".into(),
            ));
        };
        let row_sel = self.classify_dim_selector(row, row_count)?;
        let col_sel = self.classify_dim_selector(col, col_count)?;
        Ok(match (row_sel, col_sel) {
            (DimSelector::Index, DimSelector::Index) => PinpType::from(elem_type),
            (DimSelector::Index, DimSelector::Slice(len)) => PinpType::Array(elem_type, len),
            (DimSelector::Slice(len), DimSelector::Index) => PinpType::Array(elem_type, len),
            (DimSelector::Slice(row_len), DimSelector::Slice(col_len)) => {
                PinpType::Matrix(elem_type, row_len, col_len)
            }
        })
    }

    /// Classifies one dimension selector in a 2D index expression: `FullExtent`, an `Int` scalar
    /// index, or an ascending literal-bound range slice. The selector's node is typed as a side-effect.
    fn classify_dim_selector(
        &mut self,
        selector: ExprId,
        dim_len: usize,
    ) -> Result<DimSelector, SemaErrorKind> {
        // `:` — full extent of the dimension.
        if matches!(self.nodes[selector.value()], Node::FullExtent) {
            self.types[selector.value()] = PinpType::Range;
            return Ok(DimSelector::Slice(dim_len));
        }
        let selector_type = self.analyze_expr(selector)?;
        if selector_type == PinpType::Int {
            self.check_literal_index(selector, dim_len)?;
            return Ok(DimSelector::Index);
        }
        // Explicit rejection before the range path so Bool gets a clear diagnostic rather
        // than the misleading "Slice requires a literal-bound range without step."
        if selector_type == PinpType::Bool {
            return Err(SemaErrorKind::Type(
                "Matrix index must be Int, got Bool.".into(),
            ));
        }
        let bad =
            || SemaErrorKind::Type("Slice requires a literal-bound range without step.".into());
        // Must be a Range; extract bounds and validate.
        let (start, stop, step, kind) = match self.nodes[selector.value()] {
            Node::Range {
                start,
                stop,
                step,
                kind,
            } => (start, stop, step, kind),
            _ => return Err(bad()),
        };
        if step.is_some() || kind == RangeKind::DownExclusive {
            return Err(bad());
        }
        let start_val = self.int_literal(start).ok_or_else(bad)?;
        let stop_val = self.int_literal(stop).ok_or_else(bad)?;
        let oob = || SemaErrorKind::Type("Slice index out of bounds.".into());
        if start_val < 0 || stop_val < 0 {
            return Err(oob());
        }
        let start_usize = start_val as usize;
        let stop_usize = stop_val as usize;
        let slice_len = match kind {
            RangeKind::Inclusive => {
                if stop_usize >= dim_len || start_usize > stop_usize {
                    return Err(oob());
                }
                stop_usize - start_usize + 1
            }
            RangeKind::UpExclusive => {
                if stop_usize > dim_len || start_usize >= stop_usize {
                    return Err(oob());
                }
                stop_usize - start_usize
            }
            RangeKind::DownExclusive => unreachable!("handled above"),
        };
        Ok(DimSelector::Slice(slice_len))
    }

    /// Types a range-init `[literal-range]`. Every range bound must be an integer literal (so the
    /// element count is compile-time constant); the result is always `Array(Int, len)`.
    fn array_from_range_type(&mut self, range_id: ExprId) -> Result<PinpType, SemaErrorKind> {
        // Analyse the range so its sub-expressions get types — needed for codegen.
        self.analyze_expr(range_id)?;
        let count = self.literal_range_length(range_id).map_err(|err| {
            // Re-word the comprehension-centric message for the range-init context.
            match err {
                SemaErrorKind::Type(msg) => SemaErrorKind::Type(
                    msg.replace("Comprehension range", "Array range initialisation")
                        .replace("Comprehension source", "Array range initialisation"),
                ),
                other => other,
            }
        })?;
        if count == 0 {
            return Err(SemaErrorKind::Type(
                "Array range initialisation yields an empty array.".into(),
            ));
        }
        Ok(PinpType::Array(ArrayElementType::Int, count))
    }

    /// Types an index expression `array[index]`: the object must be an array. A scalar `Int`-like
    /// index yields the element type; a `Range` index (a slice) yields a sub-array.
    fn index_type(&mut self, array: ExprId, index: ExprId) -> Result<PinpType, SemaErrorKind> {
        let array_type = self.analyze_expr(array)?;
        let PinpType::Array(elem_type, array_len) = array_type else {
            return Err(SemaErrorKind::Type(format!(
                "Index requires an array, got {array_type:?}."
            )));
        };
        if matches!(self.nodes[index.value()], Node::Range { .. }) {
            return self.slice_type(index, elem_type, array_len);
        }
        let index_type = self.analyze_expr(index)?;
        if index_type != PinpType::Int {
            return Err(SemaErrorKind::Type(format!(
                "Array index must be Int, got {index_type:?}."
            )));
        }
        self.check_literal_index(index, array_len)?;
        Ok(PinpType::from(elem_type))
    }

    /// Types a slice expression `array[start..stop]` or `array[start..<stop]`. The range must be
    /// ascending (`..` or `..<`), literal-bound (no variables), non-empty, and in-bounds for the
    /// array. Returns `Array(elem_type, count)`.
    fn slice_type(
        &mut self,
        range_id: ExprId,
        elem_type: ArrayElementType,
        array_len: usize,
    ) -> Result<PinpType, SemaErrorKind> {
        let (start, stop, step, kind) = match self.nodes[range_id.value()] {
            Node::Range {
                start,
                stop,
                step,
                kind,
            } => (start, stop, step, kind),
            _ => unreachable!(),
        };
        if step.is_some() {
            return Err(SemaErrorKind::Type(
                "Array slices do not support a step.".into(),
            ));
        }
        if kind == RangeKind::DownExclusive {
            return Err(SemaErrorKind::Type(
                "Array slices must be ascending; use `..` or `..<`.".into(),
            ));
        }
        let start_val = self
            .int_literal(start)
            .ok_or_else(|| SemaErrorKind::Type("Slice bounds must be integer literals.".into()))?;
        let stop_val = self
            .int_literal(stop)
            .ok_or_else(|| SemaErrorKind::Type("Slice bounds must be integer literals.".into()))?;
        // Analyze sub-expressions so their types are recorded for codegen.
        self.analyze_expr(start)?;
        self.analyze_expr(stop)?;
        self.types[range_id.value()] = PinpType::Range;
        if start_val < 0 {
            return Err(SemaErrorKind::Type(
                "Slice start must not be negative.".into(),
            ));
        }
        let count = match kind {
            RangeKind::Inclusive => {
                if stop_val < start_val {
                    return Err(SemaErrorKind::Type(
                        "Slice stop must not be less than start.".into(),
                    ));
                }
                (stop_val - start_val + 1) as usize
            }
            RangeKind::UpExclusive => {
                if stop_val <= start_val {
                    return Err(SemaErrorKind::Type(
                        "Empty slice (start >= stop with `..<`).".into(),
                    ));
                }
                (stop_val - start_val) as usize
            }
            RangeKind::DownExclusive => unreachable!(),
        };
        let out_of_bounds = match kind {
            RangeKind::Inclusive => stop_val >= array_len as i64,
            RangeKind::UpExclusive => stop_val > array_len as i64,
            RangeKind::DownExclusive => unreachable!(),
        };
        if out_of_bounds {
            return Err(SemaErrorKind::Type(format!(
                "Slice is out of bounds for array of length {array_len}."
            )));
        }
        Ok(PinpType::Array(elem_type, count))
    }

    /// Types a member access `object.member`. Resolves the member name to a [`BuiltinMember`]
    /// variant (the single string-comparison site for members in the compiler), validates it
    /// against the object type, and records the resolved variant for codegen.
    fn member_type(
        &mut self,
        expr_id: ExprId,
        object: ExprId,
        member: SymId,
    ) -> Result<PinpType, SemaErrorKind> {
        let object_type = self.analyze_expr(object)?;
        let member_name = self.name(member);
        let Some(builtin) = BuiltinMember::from_name(member_name) else {
            return Err(SemaErrorKind::Type(format!(
                "Unknown member `.{member_name}`."
            )));
        };
        let result_type = match (builtin, object_type) {
            (BuiltinMember::Len, PinpType::Array(..)) => Ok(PinpType::Int),
            (BuiltinMember::Len, PinpType::Matrix(..)) => Ok(PinpType::Int),
            (BuiltinMember::Len, PinpType::Str) => Ok(PinpType::Int),
            (BuiltinMember::Ndim, PinpType::Array(..)) => Ok(PinpType::Int),
            (BuiltinMember::Ndim, PinpType::Matrix(..)) => Ok(PinpType::Int),
            (BuiltinMember::Rows, PinpType::Matrix(..)) => Ok(PinpType::Int),
            (BuiltinMember::Cols, PinpType::Matrix(..)) => Ok(PinpType::Int),
            (BuiltinMember::Rows, PinpType::Array(..)) => Err(SemaErrorKind::Type(
                ".rows is not defined for a 1D array. Use .len.".into(),
            )),
            (BuiltinMember::Cols, PinpType::Array(..)) => Err(SemaErrorKind::Type(
                ".cols is not defined for a 1D array. Use .len.".into(),
            )),
            (BuiltinMember::Ndim, other) => Err(SemaErrorKind::Type(format!(
                "`.ndim` is not defined on {other:?}."
            ))),
            (_, other) => Err(SemaErrorKind::Type(format!(
                "`.{member_name}` is not valid on {other:?}."
            ))),
        }?;
        self.builtin_members[expr_id.value()] = Some(builtin);
        Ok(result_type)
    }

    /// Types a comprehension `[element for var[:type] in source]`. The source must be a range with
    /// all literal bounds (so the array length is known at compile time); the loop variable is in
    /// scope while the element expression is typed.
    fn comprehension_type(
        &mut self,
        element: ExprId,
        var: SymId,
        var_type: PinpType,
        source: ExprId,
    ) -> Result<PinpType, SemaErrorKind> {
        // Only upward promotions are valid for the loop variable.
        // `Int` and `Float` are accepted; `Bool` is rejected because the range counter is an `i64`
        // and narrowing it to `i1` is not a meaningful promotion.
        if var_type == PinpType::Bool {
            return Err(SemaErrorKind::Type(
                "Cannot promote range variable to bool.".into(),
            ));
        }
        // A range counter is an `i64`; it has no meaningful promotion to a string. (Independently,
        // string values are not yet wired through codegen, so this also stops `Str` reaching it.)
        if var_type == PinpType::Str {
            return Err(SemaErrorKind::Type(
                "Cannot promote range variable to string.".into(),
            ));
        }
        let source_type = self.analyze_expr(source)?;
        if source_type != PinpType::Range {
            return Err(SemaErrorKind::Type(
                "Comprehension source must be a range.".into(),
            ));
        }
        let count = self.literal_range_length(source)?;
        if count == 0 {
            return Err(SemaErrorKind::Type(
                "Comprehension over an empty range produces no elements.".into(),
            ));
        }
        // Type the element expression with `var` in scope.
        self.scopes.push(FxHashMap::default());
        self.scopes.last_mut().unwrap().insert(var, var_type);
        let elem_pinp_type = self.analyze_expr(element)?;
        self.scopes.pop();
        Ok(PinpType::Array(
            self.to_array_element_type(elem_pinp_type)?,
            count,
        ))
    }

    /// Checks `arr[index] = value`: `target` must be a bound array. A scalar index requires
    /// `Int`-like; a range index (slice assign) fills the sub-range with a scalar broadcast.
    fn indexed_assign_check(
        &mut self,
        target: Place,
        target_span: Span,
        index: ExprId,
        value: ExprId,
        compound_op: Option<BinOp>,
    ) -> Result<(), SemaErrorKind> {
        let target_type = match target {
            Place::Local(sym_id) => match self.lookup_assign_target(sym_id) {
                Some(target_type) => target_type,
                None => {
                    let kind = SemaErrorKind::UnknownSymbol(format!(
                        "Unknown array `{}`.",
                        self.name(sym_id)
                    ));
                    return Err(self.error_at(target_span, kind));
                }
            },
            Place::Global(sym_id) => self
                .lookup_global(sym_id)
                .map_err(|kind| self.error_at(target_span, kind))?,
        };
        let PinpType::Array(elem_type, array_len) = target_type else {
            let kind = SemaErrorKind::Type("Indexed assignment requires an array target.".into());
            return Err(self.error_at(target_span, kind));
        };
        if matches!(self.nodes[index.value()], Node::Range { .. }) {
            self.slice_type(index, elem_type, array_len)?;
            let value_type = self.analyze_expr(value)?;
            let elem_pinp_type = PinpType::from(elem_type);
            return if assignable(value_type, elem_pinp_type) {
                Ok(())
            } else {
                let kind = SemaErrorKind::Type(format!(
                    "Cannot assign {value_type:?} to array element of type {elem_pinp_type:?}."
                ));
                Err(self.error_at_expr(value, kind))
            };
        }
        let index_type = self.analyze_expr(index)?;
        if index_type != PinpType::Int {
            let kind = SemaErrorKind::Type(format!("Array index must be Int, got {index_type:?}."));
            return Err(self.error_at_expr(index, kind));
        }
        self.check_literal_index(index, array_len)
            .map_err(|kind| self.error_at_expr(index, kind))?;
        let value_type = self.analyze_expr(value)?;
        let elem_pinp_type = PinpType::from(elem_type);
        // For compound assigns (`arr[i] op= rhs`), the stored value is `elem op rhs`, not `rhs`
        // itself — so check the result type of the operation, not the raw RHS type.
        let stored_type = match compound_op {
            Some(op) => self
                .bin_type(op, elem_pinp_type, value_type)
                .map_err(|kind| self.error_at_expr(value, kind))?,
            None => value_type,
        };
        if !assignable(stored_type, elem_pinp_type) {
            let kind = SemaErrorKind::Type(format!(
                "Cannot assign {stored_type:?} to array element of type {elem_pinp_type:?}."
            ));
            return Err(self.error_at_expr(value, kind));
        }
        Ok(())
    }

    /// Types a `mat[row, col] = value` statement. The target must be a Matrix; both indices must
    /// be `Int`; the value must be assignable to the element type.
    fn indexed_assign2d_check(
        &mut self,
        target: Place,
        target_span: Span,
        row: ExprId,
        col: ExprId,
        value: ExprId,
        compound_op: Option<BinOp>,
    ) -> Result<(), SemaErrorKind> {
        let target_type = match target {
            Place::Local(sym_id) => match self.lookup_assign_target(sym_id) {
                Some(target_type) => target_type,
                None => {
                    let kind = SemaErrorKind::UnknownSymbol(format!(
                        "Unknown matrix `{}`.",
                        self.name(sym_id)
                    ));
                    return Err(self.error_at(target_span, kind));
                }
            },
            Place::Global(sym_id) => self
                .lookup_global(sym_id)
                .map_err(|kind| self.error_at(target_span, kind))?,
        };
        let PinpType::Matrix(elem_type, rows, cols) = target_type else {
            let kind =
                SemaErrorKind::Type("2D indexed assignment requires a matrix target.".into());
            return Err(self.error_at(target_span, kind));
        };
        let row_type = self.analyze_expr(row)?;
        if row_type != PinpType::Int {
            let kind =
                SemaErrorKind::Type(format!("Matrix row index must be Int, got {row_type:?}."));
            return Err(self.error_at_expr(row, kind));
        }
        // TODO(KI-0003): both of these say "Array index out of bounds." for a matrix, and neither
        // says which of the two indices was out of range.
        self.check_literal_index(row, rows)
            .map_err(|kind| self.error_at_expr(row, kind))?;
        let col_type = self.analyze_expr(col)?;
        if col_type != PinpType::Int {
            let kind = SemaErrorKind::Type(format!(
                "Matrix column index must be Int, got {col_type:?}."
            ));
            return Err(self.error_at_expr(col, kind));
        }
        self.check_literal_index(col, cols)
            .map_err(|kind| self.error_at_expr(col, kind))?;
        let value_type = self.analyze_expr(value)?;
        let elem_pinp_type = PinpType::from(elem_type);
        let stored_type = match compound_op {
            Some(op) => self
                .bin_type(op, elem_pinp_type, value_type)
                .map_err(|kind| self.error_at_expr(value, kind))?,
            None => value_type,
        };
        if !assignable(stored_type, elem_pinp_type) {
            let kind = SemaErrorKind::Type(format!(
                "Cannot assign {stored_type:?} to matrix element of type {elem_pinp_type:?}."
            ));
            return Err(self.error_at_expr(value, kind));
        }
        Ok(())
    }

    /// Converts a scalar `PinpType` to its `ArrayElementType` counterpart. Non-scalar types
    /// (Void, Range, Array) cannot be array element types.
    fn to_array_element_type(
        &self,
        pinp_type: PinpType,
    ) -> Result<ArrayElementType, SemaErrorKind> {
        match pinp_type {
            PinpType::Bool => Ok(ArrayElementType::Bool),
            PinpType::Int => Ok(ArrayElementType::Int),
            PinpType::Float => Ok(ArrayElementType::Float),
            other => Err(SemaErrorKind::Type(format!(
                "{other:?} cannot be an array element type."
            ))),
        }
    }

    /// Returns the number of elements a literal-bound range produces. Requires all bounds (start,
    /// stop, and any step) to be integer literals; a variable bound is a compile-time error for
    /// comprehensions.
    fn literal_range_length(&self, source: ExprId) -> Result<usize, SemaErrorKind> {
        let Node::Range {
            start,
            stop,
            step,
            kind,
        } = self.nodes[source.value()]
        else {
            return Err(SemaErrorKind::Type(
                "Comprehension source must be a range expression.".into(),
            ));
        };
        let (start, stop, step, kind) = (start, stop, step, kind);
        let start_val = self.int_literal(start).ok_or_else(|| {
            SemaErrorKind::Type("Comprehension range start must be a literal.".into())
        })?;
        let stop_val = self.int_literal(stop).ok_or_else(|| {
            SemaErrorKind::Type("Comprehension range stop must be a literal.".into())
        })?;
        let step_val = match step {
            Some(step_id) => self.int_literal(step_id).ok_or_else(|| {
                SemaErrorKind::Type("Comprehension range step must be a literal.".into())
            })?,
            None => match kind {
                RangeKind::UpExclusive => 1,
                RangeKind::DownExclusive => -1,
                RangeKind::Inclusive => {
                    if stop_val >= start_val {
                        1
                    } else {
                        -1
                    }
                }
            },
        };
        if step_val == 0 {
            return Err(SemaErrorKind::Type("Range step cannot be zero.".into()));
        }
        let length = match kind {
            RangeKind::Inclusive => {
                if step_val > 0 && stop_val >= start_val {
                    ((stop_val - start_val) / step_val + 1) as usize
                } else if step_val < 0 && start_val >= stop_val {
                    ((start_val - stop_val) / (-step_val) + 1) as usize
                } else {
                    0
                }
            }
            RangeKind::UpExclusive => {
                if stop_val > start_val && step_val > 0 {
                    ((stop_val - start_val + step_val - 1) / step_val) as usize
                } else {
                    0
                }
            }
            RangeKind::DownExclusive => {
                if start_val > stop_val && step_val < 0 {
                    ((start_val - stop_val - step_val - 1) / (-step_val)) as usize
                } else {
                    0
                }
            }
        };
        Ok(length)
    }

    /// The type of a bare name, resolved outward to the function base.
    fn lookup_local(&self, sym_id: SymId) -> Result<PinpType, SemaErrorKind> {
        // Reading and assigning resolve a bare name the same way — outward to the function base —
        // so both share `lookup_assign_target`.
        self.lookup_assign_target(sym_id).ok_or_else(|| {
            SemaErrorKind::UnknownSymbol(format!("Unknown symbol `{}`.", self.name(sym_id)))
        })
    }

    /// The type of a `::global`, looked up in the global frame.
    fn lookup_global(&self, sym_id: SymId) -> Result<PinpType, SemaErrorKind> {
        self.scopes[0].get(&sym_id).copied().ok_or_else(|| {
            SemaErrorKind::UnknownSymbol(format!("Unknown global `::{}`.", self.name(sym_id)))
        })
    }

    /// The type a [`Place`] reads — a bare local or a `::global`. Used to resolve f-string holes.
    fn place_type(&self, place: Place) -> Result<PinpType, SemaErrorKind> {
        match place {
            Place::Local(sym_id) => self.lookup_local(sym_id),
            Place::Global(sym_id) => self.lookup_global(sym_id),
        }
    }

    /// The result type of a unary operator applied to `operand_type`.
    fn unary_type(&self, op: UnOp, operand_type: PinpType) -> Result<PinpType, SemaErrorKind> {
        match op {
            // Arithmetic negation: `Bool` promotes to `Int` (like any arithmetic use of a bool);
            // `Void`/`Range` are not numeric and are rejected.
            UnOp::Neg => match operand_type {
                PinpType::Bool | PinpType::Int => Ok(PinpType::Int),
                PinpType::Float => Ok(PinpType::Float),
                // TODO: element-wise negation on arrays/matrices may be allowed for matrix algebra later.
                PinpType::Str
                | PinpType::Void
                | PinpType::Never
                | PinpType::Range
                | PinpType::Array(_, _)
                | PinpType::Matrix(_, _, _) => Err(SemaErrorKind::Type(format!(
                    "Unary minus requires a numeric operand, got {operand_type:?}."
                ))),
            },
            UnOp::Not => {
                if operand_type == PinpType::Bool {
                    Ok(PinpType::Bool)
                } else {
                    Err(SemaErrorKind::Type(format!(
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
    ) -> Result<PinpType, SemaErrorKind> {
        use BinOp::*;
        use PinpType::*;
        // Logicals take and yield Bool only — no truthiness.
        if matches!(op, And | Or | Xor) {
            return if left_type == Bool && right_type == Bool {
                Ok(Bool)
            } else {
                Err(SemaErrorKind::Type(format!(
                    "`{op:?}` requires Bool operands, got {left_type:?} and {right_type:?}."
                )))
            };
        }
        // String operations — concatenation (`+`) and the six comparisons — sit before the numeric
        // gate, since `Str` is not numeric.
        if left_type == Str || right_type == Str {
            return string_bin_type(op, left_type, right_type);
        }
        // Arithmetic and comparison both require scalar operands — never `Void` or a `Range`.
        if !numeric(left_type) || !numeric(right_type) {
            return Err(SemaErrorKind::Type(format!(
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
                    return Err(SemaErrorKind::Type(format!(
                        "{op:?} requires Int operands."
                    )));
                }
            }
            And | Or | Xor => unreachable!("handled above"),
        })
    }

    /// Types a call: the callee must be defined, with matching arity and assignable arguments.
    /// Compiler built-ins are intercepted by name before the normal user-function path.
    fn call_type(&mut self, callee: SymId, args: &[ExprId]) -> Result<PinpType, SemaErrorKind> {
        match self.name(callee) {
            "identity" => return self.identity_call_type(args),
            "str" => return self.str_call_type(args),
            "meminfo" => return self.meminfo_call_type(args),
            _ => {}
        }
        let signature = self.funcs.get(&callee).cloned().ok_or_else(|| {
            SemaErrorKind::UnknownSymbol(format!(
                "Call to undefined function `{}`.",
                self.name(callee)
            ))
        })?;
        if args.len() != signature.params.len() {
            return Err(SemaErrorKind::Type(format!(
                "Function `{}` expects {} argument(s), got {}.",
                self.name(callee),
                signature.params.len(),
                args.len()
            )));
        }
        for (index, (&arg, &param_type)) in args.iter().zip(signature.params.iter()).enumerate() {
            let arg_type = self.analyze_expr(arg)?;
            if !assignable(arg_type, param_type) {
                return Err(SemaErrorKind::Type(format!(
                    "Argument {} of `{}`: {arg_type:?} not assignable to {param_type:?}.",
                    index + 1,
                    self.name(callee)
                )));
            }
        }
        Ok(signature.return_type)
    }

    /// Types the `identity(n, type)` built-in. `n` must be a literal integer >= 2; `type` must
    /// be the identifier `int` or `float`. Codegen reads the result `PinpType::Matrix` directly
    /// rather than processing the arg nodes, so only arg 0's type slot is filled here.
    fn identity_call_type(&mut self, args: &[ExprId]) -> Result<PinpType, SemaErrorKind> {
        if args.len() != 2 {
            return Err(SemaErrorKind::Type(
                "identity() takes exactly 2 arguments.".into(),
            ));
        }
        let size = match self.nodes[args[0].value()] {
            Node::Int(n) if n >= 2 => n as usize,
            _ => {
                return Err(SemaErrorKind::Type(
                    "identity() size must be a literal integer >= 2.".into(),
                ));
            }
        };
        self.types[args[0].value()] = PinpType::Int;
        let elem_type = match self.nodes[args[1].value()] {
            Node::Var(sym_id) => match self.names[sym_id.value()] {
                "int" => ArrayElementType::Int,
                "float" => ArrayElementType::Float,
                "bool" => {
                    return Err(SemaErrorKind::Type(
                        "identity() does not support bool element type.".into(),
                    ));
                }
                _ => {
                    return Err(SemaErrorKind::Type(
                        "identity() type must be int or float.".into(),
                    ));
                }
            },
            _ => {
                return Err(SemaErrorKind::Type(
                    "identity() type must be int or float.".into(),
                ));
            }
        };
        Ok(PinpType::Matrix(elem_type, size, size))
    }

    /// Types the `str(x)` conversion built-in: one scalar or `str` argument, yielding `Str`.
    fn str_call_type(&mut self, args: &[ExprId]) -> Result<PinpType, SemaErrorKind> {
        let [arg] = args else {
            return Err(SemaErrorKind::Type(format!(
                "`str` takes exactly 1 argument, got {}.",
                args.len()
            )));
        };
        let arg_type = self.analyze_expr(*arg)?;
        if numeric(arg_type) || arg_type == PinpType::Str {
            Ok(PinpType::Str)
        } else {
            Err(SemaErrorKind::Type(format!(
                "`str` cannot convert {arg_type:?}."
            )))
        }
    }

    /// Types the `meminfo()` diagnostic built-in: no arguments, evaluating to `Void`.
    fn meminfo_call_type(&self, args: &[ExprId]) -> Result<PinpType, SemaErrorKind> {
        if !args.is_empty() {
            return Err(SemaErrorKind::Type(format!(
                "`meminfo` takes no arguments, got {}.",
                args.len()
            )));
        }
        Ok(PinpType::Void)
    }
}
