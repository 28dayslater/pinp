// SPDX-License-Identifier: MIT

//! Statements: assignment, loops, `if`, and expressions evaluated for effect.

use super::*;
use crate::ir::List;
use crate::parser::{Block, ExprId, IfArm, Node};

/// What a body does with its trailing result expression.
#[derive(Clone, Copy)]
pub(super) enum Sink {
    /// Evaluates it for effect.
    Effect,
    /// Stores it, converted to the temporary's type, in the temporary — an if-expression's value.
    Write(LocalId, Span),
}

impl Desugarer<'_> {
    /// Appends the statements `stmt` becomes to `out`.
    pub(super) fn statement(&mut self, stmt: &Stmt, out: &mut Vec<NodeId>) {
        match stmt {
            Stmt::Expr(expr_id) => self.effect(*expr_id, out),
            Stmt::Assign {
                target_lists,
                target_spans,
                values,
            } => {
                self.assignment(target_lists, target_spans, values, false, out);
            }
            Stmt::While { cond, body } => {
                let mut header = Vec::new();
                let cond_node = self.expr(*cond, &mut header);
                let body = self.scoped(|desugarer| {
                    let mut statements = Vec::new();
                    desugarer.statements(&body.stmts, &mut statements);
                    statements
                });
                let operation = Operation::While {
                    header: self.function.arena.list(&header),
                    cond: cond_node,
                    body: self.function.arena.list(&body),
                };
                out.push(self.statement_node(operation, self.ast.span_of(*cond)));
            }
            Stmt::Loop { body, cond, until } => {
                let mut statements = self.scoped(|desugarer| {
                    let mut statements = Vec::new();
                    desugarer.statements(&body.stmts, &mut statements);
                    statements
                });
                // The condition is tested after each pass, so what computing it needs runs at the
                // end of the body.
                let cond_node = self.expr(*cond, &mut statements);
                let operation = Operation::DoWhile {
                    body: self.function.arena.list(&statements),
                    cond: cond_node,
                    until: *until,
                };
                out.push(self.statement_node(operation, self.ast.span_of(*cond)));
            }
            Stmt::For {
                var,
                var_span,
                source,
                body,
            } => match self.ast.type_of(*source) {
                PinpType::Range => {
                    // The range is evaluated before the loop's scope opens.
                    let range = self.expr(*source, out);
                    let index = self.local(Some(*var), PinpType::Int);
                    let body = self.scoped(|desugarer| {
                        let frame = desugarer.function.scopes.last_mut().expect("a scope frame");
                        frame.insert(*var, Binding::Local(index));
                        desugarer.body(body, Sink::Effect)
                    });
                    let operation = Operation::CountedLoop { index, range, body };
                    out.push(self.statement_node(operation, self.ast.span_of(*source)));
                }
                _ => self.for_over_array(&[*var], &[*var_span], *source, body, out),
            },
            Stmt::ForArray {
                binders,
                binder_spans,
                source,
                body,
            } => self.for_over_array(binders, binder_spans, *source, body, out),
            Stmt::IndexedAssign {
                target,
                target_span,
                index,
                value,
                compound_op,
            } => {
                let indices = [*index];
                self.element_assignment(*target, *target_span, &indices, *value, *compound_op, out)
            }
            Stmt::IndexedAssign2D {
                target,
                target_span,
                row,
                col,
                value,
                compound_op,
            } => {
                let indices = [*row, *col];
                self.element_assignment(*target, *target_span, &indices, *value, *compound_op, out)
            }
        }
    }

    fn statements(&mut self, stmts: &[Stmt], out: &mut Vec<NodeId>) {
        for stmt in stmts {
            self.statement(stmt, out);
        }
    }

    /// The last top-level statement, which is also the program's value when it is an expression
    /// or an assignment. Returns that value.
    pub(super) fn result_statement(
        &mut self,
        stmt: &Stmt,
        out: &mut Vec<NodeId>,
    ) -> Option<NodeId> {
        match stmt {
            Stmt::Expr(expr_id) if self.ast.type_of(*expr_id) != PinpType::Void => {
                Some(self.expr(*expr_id, out))
            }
            Stmt::Assign {
                target_lists,
                target_spans,
                values,
            } => {
                let last = self.assignment(target_lists, target_spans, values, true, out)?;
                let span = self
                    .ast
                    .span_of(*values.last().expect("an assignment has a value"));
                Some(self.read_local(last, span))
            }
            _ => {
                self.statement(stmt, out);
                None
            }
        }
    }

    /// Evaluates `expr_id` for its effect only.
    pub(super) fn effect(&mut self, expr_id: ExprId, out: &mut Vec<NodeId>) {
        if let Node::If { arms, else_block } = self.ast.node(expr_id) {
            let span = self.ast.span_of(expr_id);
            self.if_chain(arms, else_block.as_ref(), span, Sink::Effect, out);
            return;
        }
        let value = self.expr(expr_id, out);
        self.discard(value, self.ast.span_of(expr_id), out);
    }

    /// Stores `values` in the targets of every group. A single value for a single target is stored
    /// directly; otherwise every value is first evaluated into a temporary, so that no store can
    /// change a value still to be read (`a, b = b, a`). With `keep_values`, temporaries are used
    /// even for a single value, and the last one is returned.
    fn assignment(
        &mut self,
        target_lists: &[Vec<Place>],
        target_spans: &[Vec<Span>],
        values: &[ExprId],
        keep_values: bool,
        out: &mut Vec<NodeId>,
    ) -> Option<LocalId> {
        if !keep_values && target_lists.len() == 1 && values.len() == 1 {
            let value = self.expr(values[0], out);
            self.store(target_lists[0][0], target_spans[0][0], value, out);
            return None;
        }
        let mut temporaries = Vec::with_capacity(values.len());
        for &value_expr in values {
            let value = self.expr(value_expr, out);
            let temporary = self.temporary(self.ast.type_of(value_expr));
            let span = self.ast.span_of(value_expr);
            self.write(Binding::Local(temporary), value, span, out);
            temporaries.push((temporary, span));
        }
        for (group, spans) in target_lists.iter().zip(target_spans) {
            for ((place, target_span), (temporary, span)) in
                group.iter().zip(spans).zip(&temporaries)
            {
                let value = self.read_local(*temporary, *span);
                self.store(*place, *target_span, value, out);
            }
        }
        temporaries.last().map(|(temporary, _)| *temporary)
    }

    /// Writes `value` to `binding`; a string with its ownership handled.
    pub(super) fn write(
        &mut self,
        binding: Binding,
        value: NodeId,
        span: Span,
        out: &mut Vec<NodeId>,
    ) {
        if self.binding_type(binding) == PinpType::Str {
            return self.store_string(binding, value, span, out);
        }
        let operation = match binding {
            Binding::Local(local_id) => Operation::WriteLocal { local_id, value },
            Binding::Global(global_id) => Operation::WriteGlobal { global_id, value },
        };
        out.push(self.statement_node(operation, span));
    }

    /// Writes `value`, converted to the target's type, to `place`.
    fn store(&mut self, place: Place, span: Span, value: NodeId, out: &mut Vec<NodeId>) {
        let target = self.assignment_target(place, self.node_type(value));
        let value = self.convert(value, self.binding_type(target));
        self.write(target, value, span, out);
    }

    /// An `if`/`elif`/`else` chain as nested `If`s, each `elif` in the `else` body of the one
    /// before. Every body's result goes to `sink`.
    pub(super) fn if_chain(
        &mut self,
        arms: &[IfArm],
        else_block: Option<&Block>,
        span: Span,
        sink: Sink,
        out: &mut Vec<NodeId>,
    ) {
        let (arm, later_arms) = arms.split_first().expect("an if has at least one arm");
        let cond = self.expr(arm.cond, out);
        let then_body = self.body(&arm.body, sink);
        let else_body = match (later_arms, else_block) {
            ([], None) => List::EMPTY,
            ([], Some(block)) => self.body(block, sink),
            (later_arms, _) => {
                let mut statements = Vec::new();
                self.if_chain(later_arms, else_block, span, sink, &mut statements);
                self.function.arena.list(&statements)
            }
        };
        let operation = Operation::If {
            cond,
            then_body,
            else_body,
        };
        out.push(self.statement_node(operation, span));
    }

    /// A block in its own scope, its result sent to `sink`.
    pub(super) fn body(&mut self, block: &Block, sink: Sink) -> List<NodeId> {
        let mut statements = Vec::new();
        self.scoped(|desugarer| desugarer.body_into(block, sink, &mut statements));
        self.function.arena.list(&statements)
    }

    /// Appends `block`'s statements to `out`, its result sent to `sink`, in the current scope.
    pub(super) fn body_into(&mut self, block: &Block, sink: Sink, out: &mut Vec<NodeId>) {
        self.statements(&block.stmts, out);
        match (block.result, sink) {
            (None, _) => {}
            (Some(result), Sink::Effect) => self.effect(result, out),
            (Some(result), Sink::Write(temporary, span)) => {
                let value = self.expr(result, out);
                let target_type = self.binding_type(Binding::Local(temporary));
                let value = self.convert(value, target_type);
                self.write(Binding::Local(temporary), value, span, out);
            }
        }
    }
}
