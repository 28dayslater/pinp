// SPDX-License-Identifier: MIT

//! Expressions: literals, names, operators with their promotions, calls, and the expressions that
//! need statements of their own (`and`, `or`, an `if` used as a value).

use super::array::Comprehension;
use super::stmt::Sink;
use super::*;
use crate::ir::{List, RuntimeFunction};
use crate::parser::{BinOp, ExprId, FStrSegment, Node, UnOp};

impl Desugarer<'_> {
    /// The node computing `expr_id`. Any statements it needs are appended to `out`, to run before
    /// the statement the node ends up in.
    pub(super) fn expr(&mut self, expr_id: ExprId, out: &mut Vec<NodeId>) -> NodeId {
        let span = self.ast.span_of(expr_id);
        let pinp_type = self.ast.type_of(expr_id);
        match self.ast.node(expr_id) {
            Node::Int(value) => self.constant(Constant::Int(*value), pinp_type, span),
            Node::Float(value) => self.constant(Constant::Float(value.to_bits()), pinp_type, span),
            Node::Bool(value) => self.constant(Constant::Bool(*value), pinp_type, span),
            Node::Str(str_id) => self.constant(Constant::Str(*str_id), pinp_type, span),
            Node::Var(name) => {
                let binding = self.lookup(*name).expect("sema resolved every name");
                self.read(binding, span)
            }
            Node::Global(name) => self.read(Binding::Global(self.globals[name]), span),
            Node::Unary { op, operand } => {
                let operand_node = self.expr(*operand, out);
                let operand_node = match op {
                    UnOp::Neg => self.convert(operand_node, pinp_type),
                    UnOp::Not => operand_node,
                };
                let operation = Operation::Unary {
                    op: *op,
                    operand: operand_node,
                };
                self.push(operation, pinp_type, span)
            }
            Node::Bin {
                op: op @ (BinOp::And | BinOp::Or),
                lhs,
                rhs,
            } => self.short_circuit(*op, *lhs, *rhs, span, out),
            Node::Bin { op, lhs, rhs } => self.binary(*op, *lhs, *rhs, pinp_type, span, out),
            Node::Call { callee, args } => self.call(expr_id, *callee, args, out),
            Node::If { arms, else_block } => {
                let temporary = self.temporary(pinp_type);
                let else_block = else_block.as_ref();
                self.if_chain(arms, else_block, span, Sink::Write(temporary, span), out);
                self.read_local(temporary, span)
            }
            Node::FStr { segments } => self.f_string(expr_id, segments, out),
            Node::Range {
                start,
                stop,
                step,
                kind,
            } => {
                let parts: Vec<ExprId> = [*start, *stop].into_iter().chain(*step).collect();
                let parts = self.operands(&parts, out);
                let mut parts = parts
                    .into_iter()
                    .map(|part| self.convert(part, PinpType::Int));
                let (start, stop) = (parts.next(), parts.next());
                let operation = Operation::Range {
                    kind: *kind,
                    start: start.expect("a range has a start"),
                    stop: stop.expect("a range has a stop"),
                    step: parts.next(),
                };
                self.push(operation, pinp_type, span)
            }
            Node::Membership { value, range } => {
                let [value, range] = self.operands(&[*value, *range], out)[..] else {
                    unreachable!("two operands")
                };
                let value = self.convert(value, PinpType::Int);
                self.push(Operation::Membership { value, range }, pinp_type, span)
            }
            Node::ArrayLiteral { elements } => self.array_literal(expr_id, elements, out),
            Node::Index { array, index } => self.index(expr_id, *array, &[*index], out),
            Node::Member { object, .. } => self.member(expr_id, *object, out),
            Node::Comprehension {
                element,
                var,
                var_span,
                var_type,
                source,
            } => {
                let comprehension = Comprehension {
                    var: *var,
                    var_span: *var_span,
                    var_type: *var_type,
                    element: *element,
                };
                self.filled_array(expr_id, *source, Some(comprehension), out)
            }
            Node::MatrixLiteral { rows } => {
                let elements: Vec<ExprId> = rows.iter().flatten().copied().collect();
                self.array_literal(expr_id, &elements, out)
            }
            Node::Index2D { matrix, row, col } => self.index(expr_id, *matrix, &[*row, *col], out),
            Node::FullExtent => unreachable!("`:` is only a 2D index selector"),
        }
    }

    pub(super) fn read(&mut self, binding: Binding, span: Span) -> NodeId {
        match binding {
            Binding::Local(local_id) => self.read_local(local_id, span),
            Binding::Global(global_id) => {
                let pinp_type = self.binding_type(binding);
                self.push(Operation::ReadGlobal(global_id), pinp_type, span)
            }
        }
    }

    /// The nodes computing `expr_ids`, left to right; see [`operand`](Self::operand).
    pub(super) fn operands(&mut self, expr_ids: &[ExprId], out: &mut Vec<NodeId>) -> Vec<NodeId> {
        let mut nodes: Vec<NodeId> = Vec::with_capacity(expr_ids.len());
        for &expr_id in expr_ids {
            let node = self.operand(expr_id, &mut nodes, out);
            nodes.push(node);
        }
        nodes
    }

    /// The node computing `expr_id`, an operand evaluated after `earlier`. When it needs
    /// statements, the earlier operands are first saved in temporaries ahead of those statements,
    /// so they are still evaluated first.
    pub(super) fn operand(
        &mut self,
        expr_id: ExprId,
        earlier: &mut [NodeId],
        out: &mut Vec<NodeId>,
    ) -> NodeId {
        let first_hoisted = out.len();
        let node = self.expr(expr_id, out);
        if out.len() > first_hoisted {
            self.save_earlier(earlier, first_hoisted, out);
        }
        node
    }

    /// Saves every unsettled node of `earlier` in a temporary, written at `first_hoisted` in `out`
    /// — ahead of the statements hoisted from there on.
    pub(super) fn save_earlier(
        &mut self,
        earlier: &mut [NodeId],
        first_hoisted: usize,
        out: &mut Vec<NodeId>,
    ) {
        let mut insert_at = first_hoisted;
        for node in earlier.iter_mut() {
            if self.is_settled(*node) {
                continue;
            }
            let span = self.function.arena.spans[*node];
            let temporary = self.temporary(self.node_type(*node));
            let mut saving = Vec::new();
            self.write(Binding::Local(temporary), *node, span, &mut saving);
            let saved = saving.len();
            out.splice(insert_at..insert_at, saving);
            insert_at += saved;
            *node = self.read_local(temporary, span);
        }
    }

    /// Whether no statement can change the node's value: a constant, or a read of a temporary,
    /// which is written only once.
    fn is_settled(&self, node_id: NodeId) -> bool {
        let arena = &self.function.arena;
        match arena.operations[node_id] {
            Operation::Constant(_) => true,
            Operation::ReadLocal(local_id) => arena.locals[local_id].name.is_none(),
            _ => false,
        }
    }

    fn binary(
        &mut self,
        op: BinOp,
        lhs: ExprId,
        rhs: ExprId,
        result_type: PinpType,
        span: Span,
        out: &mut Vec<NodeId>,
    ) -> NodeId {
        if op == BinOp::Add && result_type == PinpType::Str {
            return self.concatenation(lhs, rhs, span, out);
        }
        let [left, right] = self.operands(&[lhs, rhs], out)[..] else {
            unreachable!("two operands")
        };
        if self.node_type(left) == PinpType::Str {
            return self.consume(vec![left, right], result_type, span, out, |_, operands| {
                Operation::Binary {
                    op,
                    lhs: operands[0],
                    rhs: operands[1],
                }
            });
        }
        self.combine(op, left, right, result_type, span)
    }

    /// `left op right`, the operands converted as the operator requires.
    pub(super) fn combine(
        &mut self,
        op: BinOp,
        mut left: NodeId,
        mut right: NodeId,
        result_type: PinpType,
        span: Span,
    ) -> NodeId {
        let (left_type, right_type) = (self.node_type(left), self.node_type(right));
        let operand_type = match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Pow => Some(result_type),
            BinOp::Div => Some(PinpType::Float),
            BinOp::IntDiv | BinOp::Mod => Some(PinpType::Int),
            // Comparisons happen at the wider type; booleans compare as the integers 0 and 1.
            // Strings compare as they are.
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                match (left_type, right_type) {
                    (PinpType::Str, _) => None,
                    (PinpType::Float, _) | (_, PinpType::Float) => Some(PinpType::Float),
                    _ => Some(PinpType::Int),
                }
            }
            BinOp::Xor => None,
            BinOp::And | BinOp::Or => unreachable!("short-circuit operators are desugared apart"),
        };
        if let Some(operand_type) = operand_type {
            left = self.convert(left, operand_type);
            right = self.convert(right, operand_type);
        }
        let operation = match op {
            BinOp::IntDiv | BinOp::Mod => Operation::IntegerDivide {
                remainder: op == BinOp::Mod,
                checked: true,
                lhs: left,
                rhs: right,
            },
            _ => Operation::Binary {
                op,
                lhs: left,
                rhs: right,
            },
        };
        self.push(operation, result_type, span)
    }

    /// `a and b` as a temporary holding `a`, overwritten with `b` only when `a` is true; `a or b`
    /// overwritten only when `a` is false. `b` is evaluated only when it decides the result.
    fn short_circuit(
        &mut self,
        op: BinOp,
        lhs: ExprId,
        rhs: ExprId,
        span: Span,
        out: &mut Vec<NodeId>,
    ) -> NodeId {
        let left = self.expr(lhs, out);
        let temporary = self.temporary(PinpType::Bool);
        out.push(self.write_local(temporary, left, span));
        let mut right_statements = Vec::new();
        let right = self.expr(rhs, &mut right_statements);
        right_statements.push(self.write_local(temporary, right, span));
        let right_body = self.function.arena.list(&right_statements);
        let (then_body, else_body) = match op {
            BinOp::And => (right_body, List::EMPTY),
            _ => (List::EMPTY, right_body),
        };
        let cond = self.read_local(temporary, span);
        let operation = Operation::If {
            cond,
            then_body,
            else_body,
        };
        out.push(self.statement_node(operation, span));
        self.read_local(temporary, span)
    }

    fn call(
        &mut self,
        expr_id: ExprId,
        callee: SymId,
        args: &[ExprId],
        out: &mut Vec<NodeId>,
    ) -> NodeId {
        match self.ast.names[callee.value()] {
            "identity" => return self.identity(expr_id, out),
            "str" => {
                let value = self.expr(args[0], out);
                return self.convert(value, PinpType::Str);
            }
            "meminfo" => {
                let operation = Operation::RuntimeCall {
                    function: RuntimeFunction::MemInfo,
                    args: List::EMPTY,
                };
                let span = self.ast.span_of(expr_id);
                return self.push(operation, PinpType::Void, span);
            }
            _ => {}
        }
        let (result_type, span) = (self.ast.type_of(expr_id), self.ast.span_of(expr_id));
        let function_id = self.function_ids[&callee];
        let arguments = self.operands(args, out);
        let mut converted = Vec::with_capacity(arguments.len());
        for (index, argument) in arguments.into_iter().enumerate() {
            let parameter_type_id = self.tables.signatures[function_id].parameters[index];
            let parameter_type = self.tables.types.get(parameter_type_id);
            converted.push(self.convert(argument, parameter_type));
        }
        let args = self.function.arena.list(&converted);
        let operation = Operation::Call {
            callee: function_id,
            args,
        };
        self.push(operation, result_type, span)
    }

    /// A chain of string `+` as one join over its parts — the left spine flattened, as a
    /// parenthesised concatenation on the right is one part — each part rendered as a string.
    fn concatenation(
        &mut self,
        lhs: ExprId,
        rhs: ExprId,
        span: Span,
        out: &mut Vec<NodeId>,
    ) -> NodeId {
        let mut parts = vec![rhs];
        let mut spine = lhs;
        while let Node::Bin {
            op: BinOp::Add,
            lhs,
            rhs,
        } = self.ast.node(spine)
            && self.ast.type_of(spine) == PinpType::Str
        {
            parts.push(*rhs);
            spine = *lhs;
        }
        parts.push(spine);
        parts.reverse();
        let parts = self.operands(&parts, out);
        self.join(parts, span, out)
    }

    /// An f-string: a join of its literal runs and its holes rendered as strings.
    fn f_string(
        &mut self,
        expr_id: ExprId,
        segments: &[FStrSegment],
        out: &mut Vec<NodeId>,
    ) -> NodeId {
        let span = self.ast.span_of(expr_id);
        if let [FStrSegment::Literal(str_id)] = segments {
            return self.constant(Constant::Str(*str_id), PinpType::Str, span);
        }
        let parts = segments
            .iter()
            .map(|segment| match segment {
                FStrSegment::Literal(str_id) => {
                    self.constant(Constant::Str(*str_id), PinpType::Str, span)
                }
                FStrSegment::Interp(Place::Local(name)) => {
                    let binding = self.lookup(*name).expect("sema resolved every name");
                    self.read(binding, span)
                }
                FStrSegment::Interp(Place::Global(name)) => {
                    self.read(Binding::Global(self.globals[name]), span)
                }
            })
            .collect();
        self.join(parts, span, out)
    }

    /// `RuntimeCall str_concat` over `parts`, each converted to a string.
    fn join(&mut self, parts: Vec<NodeId>, span: Span, out: &mut Vec<NodeId>) -> NodeId {
        let parts: Vec<NodeId> = parts
            .into_iter()
            .map(|part| self.convert(part, PinpType::Str))
            .collect();
        self.consume(parts, PinpType::Str, span, out, |desugarer, parts| {
            Operation::RuntimeCall {
                function: RuntimeFunction::StrConcat,
                args: desugarer.function.arena.list(parts),
            }
        })
    }
}
