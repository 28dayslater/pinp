// SPDX-License-Identifier: MIT

//! Arrays and matrices: literals, filled arrays (`[a..b]`, comprehensions, `identity`), element
//! access and assignment, slices, members, and `for` over either.
//!
//! An element access in the IR takes a plain 0-based index per dimension. A negative index counts
//! from the end (`a[-1]` is the last element, `m[-1, 0]` the first of the last row), so desugaring
//! spells that out: a literal index is resolved here — sema has already checked it — and any other
//! becomes `i < 0 ? i + len : i`, a `Select`, before the access checks it against the dimension.
//! A matrix is stored row by row.

use super::stmt::Sink;
use super::*;
use crate::ir::RuntimeFunction;
use crate::parser::{BinOp, Block, BuiltinMember, ExprId, Node, RangeKind, UnOp};

impl Desugarer<'_> {
    /// `[e0, e1, …]`, or `[r0e0, …; r1e0, …]` given its elements row by row.
    pub(super) fn array_literal(
        &mut self,
        expr_id: ExprId,
        elements: &[ExprId],
        out: &mut Vec<NodeId>,
    ) -> NodeId {
        if let [single] = elements
            && matches!(self.ast.node(*single), Node::Range { .. })
        {
            return self.filled_array(expr_id, *single, None, out);
        }
        let array_type = self.ast.type_of(expr_id);
        let element_type = element_type(array_type);
        let elements = self.operands(elements, out);
        let converted: Vec<NodeId> = elements
            .into_iter()
            .map(|element| self.convert(element, element_type))
            .collect();
        let elements = self.function.arena.list(&converted);
        let span = self.ast.span_of(expr_id);
        self.push(Operation::ArrayNew { elements }, array_type, span)
    }

    /// An array filled by a counted loop over `range`: with each value of the range (`[a..b]`), or
    /// with a comprehension's element computed from its variable.
    pub(super) fn filled_array(
        &mut self,
        expr_id: ExprId,
        range: ExprId,
        comprehension: Option<Comprehension>,
        out: &mut Vec<NodeId>,
    ) -> NodeId {
        let span = self.ast.span_of(expr_id);
        let array_type = self.ast.type_of(expr_id);
        let array = self.temporary(array_type);
        let allocate = self.push(Operation::ArrayAllocate { zeroed: false }, array_type, span);
        out.push(self.write_local(array, allocate, span));
        let position = self.temporary(PinpType::Int);
        let zero = self.int_constant(0, span);
        out.push(self.write_local(position, zero, span));
        let range = self.expr(range, out);
        // An `int` comprehension variable is the loop index itself; a `float` one is set from it.
        let index = match comprehension {
            Some(Comprehension {
                var,
                var_type: PinpType::Int,
                ..
            }) => self.local(Some(var), PinpType::Int),
            _ => self.temporary(PinpType::Int),
        };
        let mut body = Vec::new();
        self.scoped(|desugarer| {
            let value = match comprehension {
                None => desugarer.read_local(index, span),
                Some(Comprehension {
                    var,
                    var_span,
                    var_type,
                    element,
                }) => {
                    let variable = match var_type {
                        PinpType::Int => index,
                        _ => {
                            let variable = desugarer.local(Some(var), var_type);
                            let index_value = desugarer.read_local(index, span);
                            let converted = desugarer.convert(index_value, var_type);
                            body.push(desugarer.write_local(variable, converted, var_span));
                            variable
                        }
                    };
                    let frame = desugarer.function.scopes.last_mut().expect("a scope frame");
                    frame.insert(var, Binding::Local(variable));
                    desugarer.expr(element, &mut body)
                }
            };
            let value = desugarer.convert(value, element_type(array_type));
            let array_node = desugarer.read_local(array, span);
            let position_node = desugarer.read_local(position, span);
            let store = desugarer.element_set(array_node, &[position_node], value, false, span);
            body.push(store);
            body.push(desugarer.increment(position, span));
        });
        let body = self.function.arena.list(&body);
        let operation = Operation::CountedLoop { index, range, body };
        out.push(self.statement_node(operation, span));
        self.read_local(array, span)
    }

    /// `identity(n, t)`: a zeroed `n × n` matrix whose diagonal a counted loop sets to 1.
    pub(super) fn identity(&mut self, expr_id: ExprId, out: &mut Vec<NodeId>) -> NodeId {
        let span = self.ast.span_of(expr_id);
        let matrix_type = self.ast.type_of(expr_id);
        let matrix = self.temporary(matrix_type);
        let allocate = self.push(Operation::ArrayAllocate { zeroed: true }, matrix_type, span);
        out.push(self.write_local(matrix, allocate, span));
        let index = self.temporary(PinpType::Int);
        let size = dimensions(matrix_type)[0];
        let range = self.counting_range(size, span);
        let one = match element_type(matrix_type) {
            PinpType::Float => {
                let constant = Constant::Float(1.0f64.to_bits());
                self.constant(constant, PinpType::Float, span)
            }
            _ => self.int_constant(1, span),
        };
        let matrix_node = self.read_local(matrix, span);
        let row = self.read_local(index, span);
        let column = self.read_local(index, span);
        let store = self.element_set(matrix_node, &[row, column], one, false, span);
        let body = self.function.arena.list(&[store]);
        let operation = Operation::CountedLoop { index, range, body };
        out.push(self.statement_node(operation, span));
        self.read_local(matrix, span)
    }

    /// `array[index]`, `matrix[row, column]`, or a slice when any selector is a range or `:`.
    pub(super) fn index(
        &mut self,
        expr_id: ExprId,
        array: ExprId,
        selectors: &[ExprId],
        out: &mut Vec<NodeId>,
    ) -> NodeId {
        let span = self.ast.span_of(expr_id);
        let result_type = self.ast.type_of(expr_id);
        let array_type = self.ast.type_of(array);
        let slicing = selectors.iter().any(|&selector| {
            matches!(
                self.ast.node(selector),
                Node::Range { .. } | Node::FullExtent
            )
        });
        let mut nodes = vec![self.expr(array, out)];
        let mut checked = false;
        for (&selector, dimension) in selectors.iter().zip(dimensions(array_type)) {
            let node = match self.ast.node(selector) {
                Node::FullExtent => self.counting_range(dimension, self.ast.span_of(selector)),
                Node::Range { .. } => self.operand(selector, &mut nodes, out),
                _ => {
                    let (index, index_checked) =
                        self.element_index(selector, dimension, &mut nodes, out);
                    checked |= index_checked;
                    index
                }
            };
            nodes.push(node);
        }
        let list = self.function.arena.list(&nodes[1..]);
        let operation = match slicing {
            true => Operation::Slice {
                source: nodes[0],
                selectors: list,
            },
            false => Operation::ElementGet {
                array: nodes[0],
                indices: list,
                checked,
            },
        };
        self.push(operation, result_type, span)
    }

    /// The 0-based index for `index_expr` into a dimension of `length`, evaluated after `earlier`,
    /// and whether the access must check it: a literal is resolved now (sema has checked it),
    /// anything else counts from the end when negative and is checked.
    fn element_index(
        &mut self,
        index_expr: ExprId,
        length: usize,
        earlier: &mut [NodeId],
        out: &mut Vec<NodeId>,
    ) -> (NodeId, bool) {
        let span = self.ast.span_of(index_expr);
        if let Some(literal) = self.int_literal(index_expr) {
            let resolved = if literal < 0 {
                literal + length as i64
            } else {
                literal
            };
            return (self.int_constant(resolved, span), false);
        }
        let index = self.operand(index_expr, earlier, out);
        (self.wrapped(index, length, earlier, out), true)
    }

    /// `index < 0 ? index + length : index`. An index that is not a plain read is first computed
    /// into a temporary, since it appears three times.
    fn wrapped(
        &mut self,
        index: NodeId,
        length: usize,
        earlier: &mut [NodeId],
        out: &mut Vec<NodeId>,
    ) -> NodeId {
        let span = self.function.arena.spans[index];
        let index = match self.function.arena.operations[index] {
            Operation::ReadLocal(_) | Operation::ReadGlobal(_) => index,
            _ => {
                let first_hoisted = out.len();
                let temporary = self.temporary(PinpType::Int);
                out.push(self.write_local(temporary, index, span));
                self.save_earlier(earlier, first_hoisted, out);
                self.read_local(temporary, span)
            }
        };
        let zero = self.int_constant(0, span);
        let length = self.int_constant(length as i64, span);
        let tested = self.reread(index);
        let cond = self.combine(BinOp::Lt, tested, zero, PinpType::Bool, span);
        let shifted = self.reread(index);
        let if_true = self.combine(BinOp::Add, shifted, length, PinpType::Int, span);
        let operation = Operation::Select {
            cond,
            if_true,
            if_false: index,
        };
        self.push(operation, PinpType::Int, span)
    }

    /// A second node reading what the read `node` reads.
    fn reread(&mut self, node: NodeId) -> NodeId {
        let operation = self.function.arena.operations[node];
        let span = self.function.arena.spans[node];
        self.push(operation, self.node_type(node), span)
    }

    /// The value of an integer literal, including a negated one (`-1` is a negation of `1`).
    fn int_literal(&self, expr_id: ExprId) -> Option<i64> {
        match self.ast.node(expr_id) {
            Node::Int(value) => Some(*value),
            Node::Unary {
                op: UnOp::Neg,
                operand,
            } => match self.ast.node(*operand) {
                Node::Int(value) => Some(-value),
                _ => None,
            },
            _ => None,
        }
    }

    /// A built-in member: a constant read off the object's type — the object is not evaluated —
    /// except a string's length, which only the runtime knows.
    pub(super) fn member(
        &mut self,
        expr_id: ExprId,
        object: ExprId,
        out: &mut Vec<NodeId>,
    ) -> NodeId {
        let builtin = self
            .ast
            .builtin_member_of(expr_id)
            .expect("sema resolved every member");
        let value = match (builtin, self.ast.type_of(object)) {
            (BuiltinMember::Len, PinpType::Array(_, length)) => length,
            (BuiltinMember::Len, PinpType::Matrix(_, rows, columns)) => rows * columns,
            (BuiltinMember::Ndim, PinpType::Array(..)) => 1,
            (BuiltinMember::Ndim, PinpType::Matrix(..)) => 2,
            (BuiltinMember::Rows, PinpType::Matrix(_, rows, _)) => rows,
            (BuiltinMember::Cols, PinpType::Matrix(_, _, columns)) => columns,
            (BuiltinMember::Len, PinpType::Str) => {
                let string = self.expr(object, out);
                let span = self.ast.span_of(expr_id);
                return self.consume(vec![string], PinpType::Int, span, out, |desugarer, args| {
                    Operation::RuntimeCall {
                        function: RuntimeFunction::StrLength,
                        args: desugarer.function.arena.list(args),
                    }
                });
            }
            _ => unreachable!("sema rejected every other member"),
        };
        self.int_constant(value as i64, self.ast.span_of(expr_id))
    }

    /// `array[i] = value`, `matrix[r, c] = value`, their compound forms (`op=`), or
    /// `array[start..stop] = value`.
    pub(super) fn element_assignment(
        &mut self,
        target: Place,
        target_span: Span,
        index_exprs: &[ExprId],
        value: ExprId,
        compound_op: Option<BinOp>,
        out: &mut Vec<NodeId>,
    ) {
        let binding = match target {
            Place::Local(name) => self.lookup(name).expect("sema resolved every name"),
            Place::Global(name) => Binding::Global(self.globals[&name]),
        };
        let array_type = self.binding_type(binding);
        let element_type = element_type(array_type);
        let array = self.read(binding, target_span);
        if let [range] = index_exprs
            && matches!(self.ast.node(*range), Node::Range { .. })
        {
            self.slice_fill(array, *range, value, target_span, out);
            return;
        }
        let dimensions = dimensions(array_type);
        let Some(op) = compound_op else {
            let mut nodes = vec![array];
            let mut checked = false;
            for (&index_expr, &dimension) in index_exprs.iter().zip(&dimensions) {
                let (index, index_checked) =
                    self.element_index(index_expr, dimension, &mut nodes, out);
                checked |= index_checked;
                nodes.push(index);
            }
            let value = self.operand(value, &mut nodes, out);
            let value = self.convert(value, element_type);
            let store = self.element_set(nodes[0], &nodes[1..], value, checked, target_span);
            out.push(store);
            return;
        };
        // The array and indices are evaluated once; the get checks the indices, the set reuses
        // them.
        let saved_array = self.temporary(array_type);
        out.push(self.write_local(saved_array, array, target_span));
        let mut saved_indices = Vec::with_capacity(index_exprs.len());
        let mut checked = false;
        for (&index_expr, &dimension) in index_exprs.iter().zip(&dimensions) {
            let index_span = self.ast.span_of(index_expr);
            let (index, index_checked) = self.element_index(index_expr, dimension, &mut [], out);
            checked |= index_checked;
            let saved_index = self.temporary(PinpType::Int);
            out.push(self.write_local(saved_index, index, index_span));
            saved_indices.push((saved_index, index_span));
        }
        let array_node = self.read_local(saved_array, target_span);
        let indices = self.read_all(&saved_indices);
        let indices = self.function.arena.list(&indices);
        let get = Operation::ElementGet {
            array: array_node,
            indices,
            checked,
        };
        let mut earlier = vec![self.push(get, element_type, target_span)];
        let right = self.operand(value, &mut earlier, out);
        let result_type = arithmetic_type(op, element_type, self.node_type(right));
        let combined = self.combine(op, earlier[0], right, result_type, target_span);
        let combined = self.convert(combined, element_type);
        let array_node = self.read_local(saved_array, target_span);
        let indices = self.read_all(&saved_indices);
        out.push(self.element_set(array_node, &indices, combined, false, target_span));
    }

    fn read_all(&mut self, locals: &[(LocalId, Span)]) -> Vec<NodeId> {
        locals
            .iter()
            .map(|&(local, span)| self.read_local(local, span))
            .collect()
    }

    /// `array[start..stop] = value`: the value evaluated once, then stored into every element of
    /// the slice, whose bounds sema has checked.
    fn slice_fill(
        &mut self,
        array: NodeId,
        range: ExprId,
        value: ExprId,
        span: Span,
        out: &mut Vec<NodeId>,
    ) {
        let element_type = element_type(self.node_type(array));
        let mut earlier = vec![array];
        let value_span = self.ast.span_of(value);
        let value = self.operand(value, &mut earlier, out);
        let value = self.convert(value, element_type);
        let saved_value = self.temporary(element_type);
        out.push(self.write_local(saved_value, value, value_span));
        let range_span = self.ast.span_of(range);
        let range = self.expr(range, out);
        let index = self.temporary(PinpType::Int);
        let index_node = self.read_local(index, range_span);
        let value_node = self.read_local(saved_value, value_span);
        let store = self.element_set(earlier[0], &[index_node], value_node, false, span);
        let body = self.function.arena.list(&[store]);
        let operation = Operation::CountedLoop { index, range, body };
        out.push(self.statement_node(operation, range_span));
    }

    /// `for value in source`, or with a binder per dimension first (`for i, value in array`,
    /// `for r, c, value in matrix`): the source evaluated once, then one counted loop per dimension
    /// — rows outside columns — reading each element into the value binder.
    pub(super) fn for_over_array(
        &mut self,
        binders: &[SymId],
        binder_spans: &[Span],
        source: ExprId,
        block: &Block,
        out: &mut Vec<NodeId>,
    ) {
        let span = self.ast.span_of(source);
        let array_type = self.ast.type_of(source);
        let dimensions = dimensions(array_type);
        let source_node = self.expr(source, out);
        let array = self.temporary(array_type);
        out.push(self.write_local(array, source_node, span));
        let named = binders.len() > dimensions.len();
        let indices: Vec<LocalId> = (0..dimensions.len())
            .map(|position| match named {
                true => self.local(Some(binders[position]), PinpType::Int),
                false => self.temporary(PinpType::Int),
            })
            .collect();
        let mut body = Vec::new();
        self.scoped(|desugarer| {
            let value_binder = *binders.last().expect("a for has a value binder");
            let value = desugarer.local(Some(value_binder), element_type(array_type));
            let frame = desugarer.function.scopes.last_mut().expect("a scope frame");
            if named {
                for (binder, index) in binders.iter().zip(&indices) {
                    frame.insert(*binder, Binding::Local(*index));
                }
            }
            frame.insert(value_binder, Binding::Local(value));
            let array_node = desugarer.read_local(array, span);
            let index_nodes: Vec<NodeId> = indices
                .iter()
                .map(|&index| desugarer.read_local(index, span))
                .collect();
            let indices = desugarer.function.arena.list(&index_nodes);
            let get = Operation::ElementGet {
                array: array_node,
                indices,
                checked: false,
            };
            let element = desugarer.push(get, element_type(array_type), span);
            let value_span = *binder_spans.last().expect("a for has a value binder");
            body.push(desugarer.write_local(value, element, value_span));
            desugarer.scoped(|desugarer| desugarer.body_into(block, Sink::Effect, &mut body));
        });
        let mut body = self.function.arena.list(&body);
        for (position, (&index, &dimension)) in indices.iter().zip(&dimensions).enumerate().rev() {
            let range = self.counting_range(dimension, span);
            let operation = Operation::CountedLoop { index, range, body };
            let counted_loop = self.statement_node(operation, span);
            match position {
                0 => out.push(counted_loop),
                _ => body = self.function.arena.list(&[counted_loop]),
            }
        }
    }

    fn element_set(
        &mut self,
        array: NodeId,
        indices: &[NodeId],
        value: NodeId,
        checked: bool,
        span: Span,
    ) -> NodeId {
        let indices = self.function.arena.list(indices);
        let operation = Operation::ElementSet {
            array,
            indices,
            value,
            checked,
        };
        self.statement_node(operation, span)
    }

    /// `0..<count`.
    fn counting_range(&mut self, count: usize, span: Span) -> NodeId {
        let start = self.int_constant(0, span);
        let stop = self.int_constant(count as i64, span);
        let operation = Operation::Range {
            kind: RangeKind::UpExclusive,
            start,
            stop,
            step: None,
        };
        self.push(operation, PinpType::Range, span)
    }

    fn int_constant(&mut self, value: i64, span: Span) -> NodeId {
        self.constant(Constant::Int(value), PinpType::Int, span)
    }

    /// `local = local + 1`.
    fn increment(&mut self, local: LocalId, span: Span) -> NodeId {
        let current = self.read_local(local, span);
        let one = self.int_constant(1, span);
        let next = self.combine(BinOp::Add, current, one, PinpType::Int, span);
        self.write_local(local, next, span)
    }
}

/// A comprehension's variable and element: `[element for var: var_type in …]`.
#[derive(Clone, Copy)]
pub(super) struct Comprehension {
    pub(super) var: SymId,
    pub(super) var_span: Span,
    pub(super) var_type: PinpType,
    pub(super) element: ExprId,
}

fn element_type(array_type: PinpType) -> PinpType {
    match array_type {
        PinpType::Array(element, _) | PinpType::Matrix(element, _, _) => PinpType::from(element),
        other => unreachable!("{other:?} is not an array"),
    }
}

/// The length of each dimension: `[len]` for an array, `[rows, columns]` for a matrix.
fn dimensions(array_type: PinpType) -> Vec<usize> {
    match array_type {
        PinpType::Array(_, length) => vec![length],
        PinpType::Matrix(_, rows, columns) => vec![rows, columns],
        other => unreachable!("{other:?} is not an array"),
    }
}

/// The type of `left op right` for an arithmetic operator — what a compound assignment computes.
fn arithmetic_type(op: BinOp, left: PinpType, right: PinpType) -> PinpType {
    let int_like = |pinp_type| matches!(pinp_type, PinpType::Bool | PinpType::Int);
    match op {
        BinOp::Div => PinpType::Float,
        BinOp::IntDiv | BinOp::Mod => PinpType::Int,
        _ if int_like(left) && int_like(right) => PinpType::Int,
        _ => PinpType::Float,
    }
}
