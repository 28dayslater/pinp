// SPDX-License-Identifier: MIT

use indoc::indoc;

use super::*;
use crate::parser::ArrayElementType;

const FLOATS: PinpType = PinpType::Array(ArrayElementType::Float, 4);
const DANGLING: NodeId = NodeId::from_index(999);
/// Every node built by the tests carries this span, since `verify` rejects unspanned nodes.
const SPAN: Span = Span { start: 1, end: 2 };

/// One function's arena under construction, with the program tables it draws on.
struct Builder {
    tables: ProgramTables,
    arena: Arena,
}

impl Builder {
    fn new() -> Builder {
        Builder {
            tables: ProgramTables::default(),
            arena: Arena::default(),
        }
    }

    fn node(&mut self, operation: Operation, pinp_type: PinpType) -> NodeId {
        let type_id = self.tables.types.intern(pinp_type);
        self.arena.push(operation, type_id, SPAN)
    }

    fn statement(&mut self, operation: Operation) -> NodeId {
        self.node(operation, PinpType::Void)
    }

    fn constant(&mut self, constant: Constant, pinp_type: PinpType) -> NodeId {
        let constant_id = self.tables.constants.intern(constant);
        self.node(Operation::Constant(constant_id), pinp_type)
    }

    fn int(&mut self, value: i64) -> NodeId {
        self.constant(Constant::Int(value), PinpType::Int)
    }

    fn boolean(&mut self, value: bool) -> NodeId {
        self.constant(Constant::Bool(value), PinpType::Bool)
    }

    /// `count` distinct integer constants, for tests that only care about node identity.
    fn leaves<const COUNT: usize>(&mut self) -> [NodeId; COUNT] {
        std::array::from_fn(|index| self.int(index as i64))
    }

    fn local(&mut self, pinp_type: PinpType) -> LocalId {
        let name = SymId(self.arena.locals.len() as u32);
        let type_id = self.tables.types.intern(pinp_type);
        self.arena.locals.push(Local {
            name: Some(name),
            type_id,
        })
    }

    fn read(&mut self, local_id: LocalId) -> NodeId {
        let type_id = self.arena.locals[local_id].type_id;
        self.arena
            .push(Operation::ReadLocal(local_id), type_id, SPAN)
    }

    fn write(&mut self, local_id: LocalId, value: NodeId) -> NodeId {
        self.statement(Operation::WriteLocal { local_id, value })
    }

    fn evaluate(&mut self, value: NodeId) -> NodeId {
        self.statement(Operation::Evaluate(value))
    }

    fn list(&mut self, node_ids: &[NodeId]) -> List<NodeId> {
        self.arena.list(node_ids)
    }

    fn structured(self, body: &[NodeId], result: Option<NodeId>) -> Function<Structured> {
        let mut arena = self.arena;
        let body = arena.list(body);
        Function {
            id: FunctionId::from_index(0),
            parameter_count: 0,
            arena,
            shape: Structured { body, result },
        }
    }

    /// A flat function whose entry is the first block.
    fn flat(self, blocks: Vec<(Vec<NodeId>, Terminator)>) -> Function<Flat> {
        let mut arena = self.arena;
        let mut flat_blocks = IdVec::default();
        for (statements, terminator) in blocks {
            flat_blocks.push(BasicBlock {
                statements: arena.list(&statements),
                terminator,
            });
        }
        Function {
            id: FunctionId::from_index(0),
            parameter_count: 0,
            arena,
            shape: Flat {
                blocks: flat_blocks,
                entry: block(0),
            },
        }
    }
}

fn block(index: u32) -> BlockId {
    BlockId::from_index(index)
}

fn children(arena: &Arena, node_id: NodeId) -> Vec<NodeId> {
    let mut found = Vec::new();
    arena.for_each_child(node_id, |child| found.push(child));
    found
}

fn reads(arena: &Arena, root: NodeId) -> Vec<LocalId> {
    let mut found = Vec::new();
    arena.for_each_read(root, |local_id| found.push(local_id));
    found.sort();
    found
}

/// ```text
/// f(n: int): float is
///     total = 0.0
///     xs = [1.5, 2.5, 3.5, 4.5]
///     for i in 0..n
///         total += xs[i mod 4]
///     total
/// ```
fn loop_example() -> Function<Structured> {
    let mut builder = Builder::new();
    let n = builder.local(PinpType::Int);
    let total = builder.local(PinpType::Float);
    let xs = builder.local(FLOATS);
    let i = builder.local(PinpType::Int);

    let zero = builder.constant(Constant::Float(0.0f64.to_bits()), PinpType::Float);
    let total_init = builder.write(total, zero);

    let elements: Vec<NodeId> = [1.5f64, 2.5, 3.5, 4.5]
        .iter()
        .map(|value| builder.constant(Constant::Float(value.to_bits()), PinpType::Float))
        .collect();
    let elements = builder.list(&elements);
    let literal = builder.node(Operation::ArrayNew { elements }, FLOATS);
    let xs_init = builder.write(xs, literal);

    let start = builder.int(0);
    let stop = builder.read(n);
    let range = builder.node(
        Operation::Range {
            kind: RangeKind::Inclusive,
            start,
            stop,
            step: None,
        },
        PinpType::Range,
    );

    let total_read = builder.read(total);
    let xs_read = builder.read(xs);
    let i_read = builder.read(i);
    let four = builder.int(4);
    let index = builder.node(
        Operation::IntegerDivide {
            remainder: true,
            checked: false,
            lhs: i_read,
            rhs: four,
        },
        PinpType::Int,
    );
    let indices = builder.list(&[index]);
    let element = builder.node(
        Operation::ElementGet {
            array: xs_read,
            indices,
            checked: true,
        },
        PinpType::Float,
    );
    let sum = builder.node(
        Operation::Binary {
            op: BinOp::Add,
            lhs: total_read,
            rhs: element,
        },
        PinpType::Float,
    );
    let update = builder.write(total, sum);
    let body = builder.list(&[update]);
    let for_loop = builder.statement(Operation::CountedLoop {
        index: i,
        range,
        body,
    });

    let result = builder.read(total);
    let mut function = builder.structured(&[total_init, xs_init, for_loop], Some(result));
    function.parameter_count = 1;
    function
}

/// ```text
///   block0: x = 1; branch cond → block1, block2
///   block1: x = 2; goto block3
///   block2: goto block3
///   block3: return x
/// ```
fn diamond_example() -> Function<Flat> {
    let mut builder = Builder::new();
    let x = builder.local(PinpType::Int);
    let one = builder.int(1);
    let first_write = builder.write(x, one);
    let cond = builder.boolean(true);
    let two = builder.int(2);
    let second_write = builder.write(x, two);
    let result = builder.read(x);
    builder.flat(vec![
        (
            vec![first_write],
            Terminator::Branch {
                cond,
                then_block: block(1),
                else_block: block(2),
            },
        ),
        (vec![second_write], Terminator::Goto(block(3))),
        (vec![], Terminator::Goto(block(3))),
        (vec![], Terminator::Return(Some(result))),
    ])
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

#[test]
fn operations_and_terminators_are_sixteen_bytes() {
    assert_eq!(size_of::<Operation>(), 16);
    assert_eq!(size_of::<Terminator>(), 16);
}

#[test]
fn ids_lists_and_optional_ids_are_four_bytes() {
    assert_eq!(size_of::<NodeId>(), 4);
    assert_eq!(size_of::<LocalId>(), 4);
    assert_eq!(size_of::<TypeId>(), 4);
    assert_eq!(size_of::<ConstantId>(), 4);
    assert_eq!(size_of::<BlockId>(), 4);
    assert_eq!(size_of::<List<NodeId>>(), 4);
    assert_eq!(size_of::<List<BlockId>>(), 4);
    assert_eq!(size_of::<Option<NodeId>>(), 4);
    assert_eq!(size_of::<Option<TypeId>>(), 4);
}

// ---------------------------------------------------------------------------
// Type interning
// ---------------------------------------------------------------------------

#[test]
fn interning_a_type_twice_yields_one_id() {
    let mut types = TypeTable::default();
    assert_eq!(types.intern(FLOATS), types.intern(FLOATS));
    assert_eq!(types.intern(PinpType::Int), types.intern(PinpType::Int));
}

#[test]
fn every_type_distinction_yields_a_distinct_id() {
    use ArrayElementType as Element;
    let distinct = [
        PinpType::Bool,
        PinpType::Int,
        PinpType::Float,
        PinpType::Str,
        PinpType::Void,
        PinpType::Range,
        PinpType::Array(Element::Int, 4),
        PinpType::Array(Element::Float, 4),
        PinpType::Array(Element::Bool, 4),
        PinpType::Array(Element::Int, 5),
        PinpType::Matrix(Element::Int, 2, 2),
        PinpType::Matrix(Element::Int, 1, 4),
        PinpType::Matrix(Element::Int, 4, 1),
        PinpType::Matrix(Element::Float, 2, 2),
    ];
    let mut types = TypeTable::default();
    let mut ids: Vec<TypeId> = distinct.iter().map(|each| types.intern(*each)).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), distinct.len());
}

#[test]
fn an_interned_type_reads_back_unchanged() {
    let mut types = TypeTable::default();
    let matrix = PinpType::Matrix(ArrayElementType::Bool, 3, 7);
    let type_id = types.intern(matrix);
    assert_eq!(types.get(type_id), matrix);
}

// ---------------------------------------------------------------------------
// Constant interning
// ---------------------------------------------------------------------------

#[test]
fn interning_a_constant_twice_yields_one_id() {
    let mut constants = ConstantPool::default();
    assert_eq!(
        constants.intern(Constant::Int(4)),
        constants.intern(Constant::Int(4))
    );
}

#[test]
fn nan_interns_to_itself() {
    let mut constants = ConstantPool::default();
    let nan = Constant::Float(f64::NAN.to_bits());
    assert_eq!(constants.intern(nan), constants.intern(nan));
}

#[test]
fn nan_payloads_stay_distinct() {
    let mut constants = ConstantPool::default();
    let quiet = f64::NAN.to_bits();
    let other_payload = quiet | 1;
    assert!(f64::from_bits(other_payload).is_nan());
    assert_ne!(
        constants.intern(Constant::Float(quiet)),
        constants.intern(Constant::Float(other_payload))
    );
}

#[test]
fn negative_zero_stays_distinct_from_zero() {
    let mut constants = ConstantPool::default();
    assert_ne!(
        constants.intern(Constant::Float(0.0f64.to_bits())),
        constants.intern(Constant::Float((-0.0f64).to_bits()))
    );
}

#[test]
fn zeros_of_different_kinds_stay_distinct() {
    let mut constants = ConstantPool::default();
    let mut ids = vec![
        constants.intern(Constant::Int(0)),
        constants.intern(Constant::Bool(false)),
        constants.intern(Constant::Float(0.0f64.to_bits())),
        constants.intern(Constant::Str(StrId(0))),
    ];
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 4);
}

#[test]
fn distinct_strings_stay_distinct() {
    let mut constants = ConstantPool::default();
    assert_ne!(
        constants.intern(Constant::Str(StrId(0))),
        constants.intern(Constant::Str(StrId(1)))
    );
}

#[test]
fn an_interned_constant_reads_back_unchanged() {
    let mut constants = ConstantPool::default();
    let odd_nan = Constant::Float(f64::NAN.to_bits() | 0x1234);
    let constant_id = constants.intern(odd_nan);
    assert_eq!(constants.get(constant_id), odd_nan);
    let minimum = Constant::Int(i64::MIN);
    let constant_id = constants.intern(minimum);
    assert_eq!(constants.get(constant_id), minimum);
}

// ---------------------------------------------------------------------------
// Arena
// ---------------------------------------------------------------------------

#[test]
fn a_pushed_node_records_its_type_and_span() {
    let mut tables = ProgramTables::default();
    let mut arena = Arena::default();
    let float_id = tables.types.intern(PinpType::Float);
    let constant_id = tables.constants.intern(Constant::Float(2.5f64.to_bits()));
    let span = Span { start: 3, end: 6 };
    let node_id = arena.push(Operation::Constant(constant_id), float_id, span);
    assert_eq!(arena.types[node_id], Some(float_id));
    assert_eq!(arena.spans[node_id], span);
}

#[test]
fn a_node_without_side_table_entries_reads_as_untyped_and_unknown() {
    let mut arena = Arena::default();
    let node_id = arena
        .operations
        .push(Operation::ReadGlobal(GlobalId::from_index(0)));
    assert_eq!(arena.types[node_id], None);
    assert_eq!(arena.spans[node_id], Span::UNKNOWN);
}

#[test]
fn a_list_reads_back_in_order() {
    let mut builder = Builder::new();
    let [first, second, third] = builder.leaves();
    let list = builder.list(&[third, first, second]);
    assert_eq!(builder.arena.items(list), &[third, first, second]);
}

#[test]
fn an_empty_list_reads_back_empty() {
    let mut arena = Arena::default();
    let list = arena.list(&[]);
    assert!(arena.items(list).is_empty());
    assert!(arena.items(List::<NodeId>::EMPTY).is_empty());
}

#[test]
fn lists_in_one_pool_do_not_overlap() {
    let mut builder = Builder::new();
    let [first, second, third, fourth] = builder.leaves();
    let left = builder.list(&[first, second]);
    let right = builder.list(&[third, fourth]);
    assert_eq!(builder.arena.items(left), &[first, second]);
    assert_eq!(builder.arena.items(right), &[third, fourth]);
}

#[test]
fn clear_restarts_node_and_local_ids() {
    let mut builder = Builder::new();
    builder.leaves::<3>();
    builder.local(PinpType::Int);
    builder.arena.clear();
    assert!(builder.arena.operations.is_empty());
    assert!(builder.arena.locals.is_empty());
    assert_eq!(builder.int(7), NodeId::from_index(0));
    assert_eq!(builder.local(PinpType::Int), LocalId::from_index(0));
}

#[test]
fn clear_forgets_side_table_entries() {
    let mut builder = Builder::new();
    let span = Span { start: 10, end: 12 };
    let int_id = builder.tables.types.intern(PinpType::Int);
    let constant_id = builder.tables.constants.intern(Constant::Int(1));
    builder
        .arena
        .push(Operation::Constant(constant_id), int_id, span);
    builder.arena.clear();
    let reused = builder
        .arena
        .operations
        .push(Operation::Constant(constant_id));
    assert_eq!(builder.arena.types[reused], None);
    assert_eq!(builder.arena.spans[reused], Span::UNKNOWN);
}

#[test]
fn clear_empties_the_list_pool() {
    let mut builder = Builder::new();
    let [first, second] = builder.leaves();
    builder.list(&[first, second]);
    builder.arena.clear();
    assert_eq!(builder.arena.lists.slot_count(), 0);
}

#[test]
fn node_and_block_lists_share_one_pool() {
    let mut builder = Builder::new();
    let [first, second] = builder.leaves();
    let nodes = builder.list(&[first, second]);
    let blocks = builder.arena.list(&[block(2), block(0), block(1)]);
    let more_nodes = builder.list(&[second]);
    assert_eq!(builder.arena.items(nodes), &[first, second]);
    assert_eq!(builder.arena.items(blocks), &[block(2), block(0), block(1)]);
    assert_eq!(builder.arena.items(more_nodes), &[second]);
}

// ---------------------------------------------------------------------------
// Child walk
// ---------------------------------------------------------------------------

#[test]
fn leaves_have_no_children() {
    let mut builder = Builder::new();
    let local_id = builder.local(PinpType::Int);
    let leaves = [
        builder.int(1),
        builder.read(local_id),
        builder.node(
            Operation::ReadGlobal(GlobalId::from_index(0)),
            PinpType::Int,
        ),
        builder.node(Operation::ArrayAllocate { zeroed: true }, FLOATS),
    ];
    for leaf in leaves {
        assert!(children(&builder.arena, leaf).is_empty());
    }
}

#[test]
fn single_child_operations_visit_their_child() {
    let mut builder = Builder::new();
    let local_id = builder.local(PinpType::Int);
    let [value] = builder.leaves();
    let operations = [
        Operation::WriteLocal { local_id, value },
        Operation::WriteGlobal {
            global_id: GlobalId::from_index(0),
            value,
        },
        Operation::Evaluate(value),
        Operation::Unary {
            op: UnOp::Neg,
            operand: value,
        },
        Operation::Convert { operand: value },
        Operation::Load {
            address: value,
            offset: 8,
            stored: ScalarKind::Word,
        },
    ];
    for operation in operations {
        let node_id = builder.statement(operation);
        assert_eq!(children(&builder.arena, node_id), vec![value]);
    }
}

#[test]
fn two_operand_operations_visit_left_then_right() {
    let mut builder = Builder::new();
    let [lhs, rhs] = builder.leaves();
    let operations = [
        Operation::Binary {
            op: BinOp::Sub,
            lhs,
            rhs,
        },
        Operation::IntegerDivide {
            remainder: false,
            checked: true,
            lhs,
            rhs,
        },
        Operation::Membership {
            value: lhs,
            range: rhs,
        },
        Operation::ElementAddress {
            array: lhs,
            index: rhs,
        },
        Operation::Store {
            address: lhs,
            value: rhs,
            offset: 0,
            stored: ScalarKind::Byte,
        },
    ];
    for operation in operations {
        let node_id = builder.statement(operation);
        assert_eq!(children(&builder.arena, node_id), vec![lhs, rhs]);
    }
}

#[test]
fn list_operations_visit_their_list_in_order() {
    let mut builder = Builder::new();
    let [first, second, third] = builder.leaves();
    let args = builder.list(&[third, first, second]);
    let operations = [
        Operation::Call {
            callee: FunctionId::from_index(0),
            args,
        },
        Operation::RuntimeCall {
            function: RuntimeFunction::StrConcat,
            args,
        },
        Operation::ArrayNew { elements: args },
    ];
    for operation in operations {
        let node_id = builder.statement(operation);
        assert_eq!(
            children(&builder.arena, node_id),
            vec![third, first, second]
        );
    }
    let empty = List::EMPTY;
    let no_args = builder.statement(Operation::Call {
        callee: FunctionId::from_index(0),
        args: empty,
    });
    assert!(children(&builder.arena, no_args).is_empty());
}

#[test]
fn a_range_visits_its_step_only_when_present() {
    let mut builder = Builder::new();
    let [start, stop, step] = builder.leaves();
    let without_step = builder.node(
        Operation::Range {
            kind: RangeKind::UpExclusive,
            start,
            stop,
            step: None,
        },
        PinpType::Range,
    );
    let with_step = builder.node(
        Operation::Range {
            kind: RangeKind::DownExclusive,
            start,
            stop,
            step: Some(step),
        },
        PinpType::Range,
    );
    assert_eq!(children(&builder.arena, without_step), vec![start, stop]);
    assert_eq!(children(&builder.arena, with_step), vec![start, stop, step]);
}

#[test]
fn element_access_visits_the_array_then_indices_then_the_value() {
    let mut builder = Builder::new();
    let [array, row, column, value] = builder.leaves();
    let indices = builder.list(&[row, column]);
    let get = builder.node(
        Operation::ElementGet {
            array,
            indices,
            checked: true,
        },
        PinpType::Int,
    );
    let set = builder.statement(Operation::ElementSet {
        array,
        indices,
        value,
        checked: true,
    });
    let slice = builder.node(
        Operation::Slice {
            source: array,
            selectors: indices,
        },
        FLOATS,
    );
    assert_eq!(children(&builder.arena, get), vec![array, row, column]);
    assert_eq!(
        children(&builder.arena, set),
        vec![array, row, column, value]
    );
    assert_eq!(children(&builder.arena, slice), vec![array, row, column]);
}

#[test]
fn control_flow_visits_children_in_execution_order() {
    let mut builder = Builder::new();
    let local_id = builder.local(PinpType::Int);
    let [cond, first, second, third, range] = builder.leaves();
    let first_list = builder.list(&[first]);
    let second_list = builder.list(&[second, third]);

    let if_node = builder.statement(Operation::If {
        cond,
        then_body: first_list,
        else_body: second_list,
    });
    let while_node = builder.statement(Operation::While {
        header: first_list,
        cond,
        body: second_list,
    });
    let do_while = builder.statement(Operation::DoWhile {
        body: second_list,
        cond,
        until: true,
    });
    let counted_loop = builder.statement(Operation::CountedLoop {
        index: local_id,
        range,
        body: second_list,
    });

    assert_eq!(
        children(&builder.arena, if_node),
        vec![cond, first, second, third]
    );
    assert_eq!(
        children(&builder.arena, while_node),
        vec![first, cond, second, third]
    );
    assert_eq!(
        children(&builder.arena, do_while),
        vec![second, third, cond]
    );
    assert_eq!(
        children(&builder.arena, counted_loop),
        vec![range, second, third]
    );
}

// ---------------------------------------------------------------------------
// Read walk
// ---------------------------------------------------------------------------

#[test]
fn reads_are_found_inside_nested_bodies_index_lists_and_steps() {
    let mut builder = Builder::new();
    let [array, index, step, cond, target] = [
        builder.local(FLOATS),
        builder.local(PinpType::Int),
        builder.local(PinpType::Int),
        builder.local(PinpType::Bool),
        builder.local(PinpType::Float),
    ];
    let array_read = builder.read(array);
    let index_read = builder.read(index);
    let indices = builder.list(&[index_read]);
    let element = builder.node(
        Operation::ElementGet {
            array: array_read,
            indices,
            checked: true,
        },
        PinpType::Float,
    );
    let inner_write = builder.write(target, element);
    let [start, stop] = builder.leaves();
    let step_read = builder.read(step);
    let range = builder.node(
        Operation::Range {
            kind: RangeKind::Inclusive,
            start,
            stop,
            step: Some(step_read),
        },
        PinpType::Range,
    );
    let body = builder.list(&[inner_write]);
    let counted_loop = builder.statement(Operation::CountedLoop { index, range, body });
    let cond_read = builder.read(cond);
    let then_body = builder.list(&[counted_loop]);
    let if_node = builder.statement(Operation::If {
        cond: cond_read,
        then_body,
        else_body: List::EMPTY,
    });
    assert_eq!(
        reads(&builder.arena, if_node),
        vec![array, index, step, cond]
    );
}

#[test]
fn a_local_read_twice_is_reported_twice() {
    let mut builder = Builder::new();
    let local_id = builder.local(PinpType::Int);
    let lhs = builder.read(local_id);
    let rhs = builder.read(local_id);
    let product = builder.node(
        Operation::Binary {
            op: BinOp::Mul,
            lhs,
            rhs,
        },
        PinpType::Int,
    );
    assert_eq!(reads(&builder.arena, product), vec![local_id, local_id]);
}

#[test]
fn globals_write_targets_and_loop_indices_are_not_reads() {
    let mut builder = Builder::new();
    let written = builder.local(PinpType::Int);
    let index = builder.local(PinpType::Int);
    let global = builder.node(
        Operation::ReadGlobal(GlobalId::from_index(0)),
        PinpType::Int,
    );
    let write = builder.write(written, global);
    let [start, stop] = builder.leaves();
    let range = builder.node(
        Operation::Range {
            kind: RangeKind::Inclusive,
            start,
            stop,
            step: None,
        },
        PinpType::Range,
    );
    let body = builder.list(&[write]);
    let counted_loop = builder.statement(Operation::CountedLoop { index, range, body });
    assert!(reads(&builder.arena, counted_loop).is_empty());
}

// ---------------------------------------------------------------------------
// Operation properties
// ---------------------------------------------------------------------------

/// One sample of every operation, with whether it is allowed at the structured and flat levels
/// and whether it is a statement. Partial lowering: the lowered operations (`RuntimeCall`,
/// `ElementAddress`, `Load`, `Store`) are allowed at both levels; only structured control flow and
/// whole-array operations are barred from the flat level.
fn every_operation(builder: &mut Builder) -> Vec<(Operation, bool, bool, bool)> {
    let local_id = builder.local(PinpType::Int);
    let [value, other] = builder.leaves();
    let list = builder.list(&[value]);
    let constant_id = builder.tables.constants.intern(Constant::Int(0));
    use Operation::*;
    vec![
        (Constant(constant_id), true, true, false),
        (ReadLocal(local_id), true, true, false),
        (ReadGlobal(GlobalId::from_index(0)), true, true, false),
        (WriteLocal { local_id, value }, true, true, true),
        (
            WriteGlobal {
                global_id: GlobalId::from_index(0),
                value,
            },
            true,
            true,
            true,
        ),
        (Evaluate(value), true, true, true),
        (
            Unary {
                op: UnOp::Not,
                operand: value,
            },
            true,
            true,
            false,
        ),
        (
            Binary {
                op: BinOp::Add,
                lhs: value,
                rhs: other,
            },
            true,
            true,
            false,
        ),
        (
            IntegerDivide {
                remainder: true,
                checked: true,
                lhs: value,
                rhs: other,
            },
            true,
            true,
            false,
        ),
        (Convert { operand: value }, true, true, false),
        (
            Call {
                callee: FunctionId::from_index(0),
                args: list,
            },
            true,
            true,
            false,
        ),
        (
            Range {
                kind: RangeKind::Inclusive,
                start: value,
                stop: other,
                step: None,
            },
            true,
            false,
            false,
        ),
        (
            Membership {
                value,
                range: other,
            },
            true,
            false,
            false,
        ),
        (ArrayNew { elements: list }, true, false, false),
        (ArrayAllocate { zeroed: false }, true, false, false),
        (
            ElementGet {
                array: value,
                indices: list,
                checked: true,
            },
            true,
            false,
            false,
        ),
        (
            ElementSet {
                array: value,
                indices: list,
                value: other,
                checked: true,
            },
            true,
            false,
            true,
        ),
        (
            Slice {
                source: value,
                selectors: list,
            },
            true,
            false,
            false,
        ),
        (
            If {
                cond: value,
                then_body: list,
                else_body: list,
            },
            true,
            false,
            true,
        ),
        (
            While {
                header: list,
                cond: value,
                body: list,
            },
            true,
            false,
            true,
        ),
        (
            DoWhile {
                body: list,
                cond: value,
                until: false,
            },
            true,
            false,
            true,
        ),
        (
            CountedLoop {
                index: local_id,
                range: value,
                body: list,
            },
            true,
            false,
            true,
        ),
        (
            RuntimeCall {
                function: RuntimeFunction::StrLength,
                args: list,
            },
            true,
            true,
            false,
        ),
        (
            ElementAddress {
                array: value,
                index: other,
            },
            true,
            true,
            false,
        ),
        (
            Load {
                address: value,
                offset: 0,
                stored: ScalarKind::Word,
            },
            true,
            true,
            false,
        ),
        (
            Store {
                address: value,
                value: other,
                offset: 16,
                stored: ScalarKind::Byte,
            },
            true,
            true,
            true,
        ),
    ]
}

#[test]
fn every_operation_belongs_to_its_levels() {
    let mut builder = Builder::new();
    for (operation, structured, flat, _) in every_operation(&mut builder) {
        assert_eq!(
            operation.allowed_in(Level::Structured),
            structured,
            "{operation:?}"
        );
        assert_eq!(operation.allowed_in(Level::Flat), flat, "{operation:?}");
    }
}

#[test]
fn every_operation_is_either_a_statement_or_a_value() {
    let mut builder = Builder::new();
    for (operation, _, _, statement) in every_operation(&mut builder) {
        assert_eq!(operation.is_statement(), statement, "{operation:?}");
    }
}

#[test]
fn short_circuit_and_integer_division_are_never_binary() {
    let mut builder = Builder::new();
    let [lhs, rhs] = builder.leaves();
    for op in [BinOp::And, BinOp::Or, BinOp::IntDiv, BinOp::Mod] {
        let binary = Operation::Binary { op, lhs, rhs };
        assert!(!binary.allowed_in(Level::Structured), "{op:?}");
        assert!(!binary.allowed_in(Level::Flat), "{op:?}");
    }
    // `xor` evaluates both operands, so it stays an ordinary binary operation.
    let xor = Operation::Binary {
        op: BinOp::Xor,
        lhs,
        rhs,
    };
    assert!(xor.allowed_in(Level::Structured));
    assert!(xor.allowed_in(Level::Flat));
}

#[test]
fn checked_operations_trap_only_while_checked() {
    let mut builder = Builder::new();
    let [array, index, value] = builder.leaves();
    let indices = builder.list(&[index]);
    for checked in [true, false] {
        let operations = [
            Operation::IntegerDivide {
                remainder: false,
                checked,
                lhs: array,
                rhs: index,
            },
            Operation::ElementGet {
                array,
                indices,
                checked,
            },
            Operation::ElementSet {
                array,
                indices,
                value,
                checked,
            },
        ];
        for operation in operations {
            assert_eq!(operation.may_trap(), checked, "{operation:?}");
        }
    }
}

#[test]
fn a_range_traps_only_with_an_explicit_step() {
    let mut builder = Builder::new();
    let [start, stop, step] = builder.leaves();
    let implicit = Operation::Range {
        kind: RangeKind::Inclusive,
        start,
        stop,
        step: None,
    };
    let explicit = Operation::Range {
        kind: RangeKind::Inclusive,
        start,
        stop,
        step: Some(step),
    };
    assert!(!implicit.may_trap(), "an implicit step is ±1, never zero");
    assert!(explicit.may_trap(), "an explicit step may be zero");
}

#[test]
fn calls_and_slices_may_trap() {
    let mut builder = Builder::new();
    let [source, index] = builder.leaves();
    let list = builder.list(&[index]);
    let call = Operation::Call {
        callee: FunctionId::from_index(0),
        args: list,
    };
    let slice = Operation::Slice {
        source,
        selectors: list,
    };
    assert!(call.may_trap());
    assert!(slice.may_trap(), "a selector may be out of range");
}

#[test]
fn pure_operations_never_trap() {
    let mut builder = Builder::new();
    let local_id = builder.local(PinpType::Int);
    let [lhs, rhs] = builder.leaves();
    let constant_id = builder.tables.constants.intern(Constant::Int(0));
    let operations = [
        Operation::Constant(constant_id),
        Operation::ReadLocal(local_id),
        Operation::Binary {
            op: BinOp::Div,
            lhs,
            rhs,
        },
        Operation::Binary {
            op: BinOp::Pow,
            lhs,
            rhs,
        },
        Operation::Unary {
            op: UnOp::Neg,
            operand: lhs,
        },
        Operation::Convert { operand: lhs },
        Operation::Membership {
            value: lhs,
            range: rhs,
        },
        Operation::ElementAddress {
            array: lhs,
            index: rhs,
        },
        Operation::Load {
            address: lhs,
            offset: 0,
            stored: ScalarKind::Word,
        },
    ];
    for operation in operations {
        assert!(!operation.may_trap(), "{operation:?}");
    }
}

#[test]
fn effects_are_writes_calls_allocations_and_stores() {
    let mut builder = Builder::new();
    let with_effects: Vec<bool> = every_operation(&mut builder)
        .iter()
        .map(|(operation, ..)| operation.has_effects())
        .collect();
    let expected: Vec<bool> = every_operation(&mut builder)
        .iter()
        .map(|(operation, ..)| {
            matches!(
                operation,
                Operation::WriteLocal { .. }
                    | Operation::WriteGlobal { .. }
                    | Operation::Call { .. }
                    | Operation::RuntimeCall { .. }
                    | Operation::ArrayNew { .. }
                    | Operation::ArrayAllocate { .. }
                    | Operation::ElementSet { .. }
                    | Operation::Store { .. }
            )
        })
        .collect();
    assert_eq!(with_effects, expected);
}

#[test]
fn a_pure_tree_is_removable() {
    let mut builder = Builder::new();
    let local_id = builder.local(PinpType::Int);
    let read = builder.read(local_id);
    let [four] = builder.leaves();
    let remainder = builder.node(
        Operation::IntegerDivide {
            remainder: true,
            checked: false,
            lhs: read,
            rhs: four,
        },
        PinpType::Int,
    );
    let negated = builder.node(
        Operation::Unary {
            op: UnOp::Neg,
            operand: remainder,
        },
        PinpType::Int,
    );
    assert!(builder.arena.is_removable(negated));
}

#[test]
fn a_trap_deep_inside_a_tree_makes_it_unremovable() {
    let mut builder = Builder::new();
    let [array, index, other] = builder.leaves();
    let indices = builder.list(&[index]);
    let element = builder.node(
        Operation::ElementGet {
            array,
            indices,
            checked: true,
        },
        PinpType::Int,
    );
    let inner = builder.node(
        Operation::Binary {
            op: BinOp::Add,
            lhs: other,
            rhs: element,
        },
        PinpType::Int,
    );
    let outer = builder.node(
        Operation::Unary {
            op: UnOp::Neg,
            operand: inner,
        },
        PinpType::Int,
    );
    assert!(!builder.arena.is_removable(outer));
}

#[test]
fn clearing_the_checked_flag_makes_an_access_removable() {
    let mut builder = Builder::new();
    let [array, index] = builder.leaves();
    let indices = builder.list(&[index]);
    let element = builder.node(
        Operation::ElementGet {
            array,
            indices,
            checked: false,
        },
        PinpType::Int,
    );
    assert!(builder.arena.is_removable(element));
}

#[test]
fn an_effect_deep_inside_a_tree_makes_it_unremovable() {
    let mut builder = Builder::new();
    let [first] = builder.leaves();
    let args = builder.list(&[first]);
    let call = builder.node(
        Operation::Call {
            callee: FunctionId::from_index(0),
            args,
        },
        PinpType::Int,
    );
    let converted = builder.node(Operation::Convert { operand: call }, PinpType::Float);
    assert!(!builder.arena.is_removable(converted));
    let elements = builder.list(&[first]);
    let literal = builder.node(
        Operation::ArrayNew { elements },
        PinpType::Array(ArrayElementType::Int, 1),
    );
    assert!(!builder.arena.is_removable(literal));
}

// ---------------------------------------------------------------------------
// Verifier: valid functions
// ---------------------------------------------------------------------------

#[test]
fn a_valid_structured_function_verifies() {
    let function = loop_example();
    assert_eq!(verify(&function), Ok(()));
    assert_eq!(
        function.arena.operations.len(),
        21,
        "one node per pinp construct"
    );
}

#[test]
fn a_valid_flat_function_verifies() {
    assert_eq!(verify(&diamond_example()), Ok(()));
}

#[test]
fn an_empty_structured_function_verifies() {
    assert_eq!(verify(&Builder::new().structured(&[], None)), Ok(()));
}

#[test]
fn an_empty_flat_function_verifies() {
    let function = Builder::new().flat(vec![(vec![], Terminator::Return(None))]);
    assert_eq!(verify(&function), Ok(()));
}

#[test]
fn nested_bodies_of_every_loop_kind_verify() {
    let mut builder = Builder::new();
    let local_id = builder.local(PinpType::Int);
    let index = builder.local(PinpType::Int);
    let mut write_one = |builder: &mut Builder| {
        let one = builder.int(1);
        builder.write(local_id, one)
    };
    let header_write = write_one(&mut builder);
    let header = builder.list(&[header_write]);
    let cond = builder.boolean(false);
    let body_write = write_one(&mut builder);
    let body = builder.list(&[body_write]);
    let while_node = builder.statement(Operation::While { header, cond, body });

    let cond = builder.boolean(true);
    let body_write = write_one(&mut builder);
    let body = builder.list(&[body_write]);
    let do_while = builder.statement(Operation::DoWhile {
        body,
        cond,
        until: true,
    });

    let [start, stop] = builder.leaves();
    let range = builder.node(
        Operation::Range {
            kind: RangeKind::Inclusive,
            start,
            stop,
            step: None,
        },
        PinpType::Range,
    );
    let body = builder.list(&[while_node, do_while]);
    let counted_loop = builder.statement(Operation::CountedLoop { index, range, body });
    assert_eq!(verify(&builder.structured(&[counted_loop], None)), Ok(()));
}

// ---------------------------------------------------------------------------
// Verifier: trees, not DAGs
// ---------------------------------------------------------------------------

#[test]
fn a_node_shared_within_a_statement_is_rejected() {
    let mut builder = Builder::new();
    let local_id = builder.local(PinpType::Int);
    let [shared] = builder.leaves();
    let doubled = builder.node(
        Operation::Binary {
            op: BinOp::Add,
            lhs: shared,
            rhs: shared,
        },
        PinpType::Int,
    );
    let write = builder.write(local_id, doubled);
    assert_eq!(
        verify(&builder.structured(&[write], None)),
        Err(VerifyError::SharedNode(shared))
    );
}

#[test]
fn a_node_shared_across_statements_is_rejected() {
    let mut builder = Builder::new();
    let local_id = builder.local(PinpType::Int);
    let [shared] = builder.leaves();
    let first = builder.write(local_id, shared);
    let second = builder.write(local_id, shared);
    assert_eq!(
        verify(&builder.structured(&[first, second], None)),
        Err(VerifyError::SharedNode(shared))
    );
}

#[test]
fn a_statement_listed_twice_is_rejected() {
    let mut builder = Builder::new();
    let local_id = builder.local(PinpType::Int);
    let [value] = builder.leaves();
    let write = builder.write(local_id, value);
    assert_eq!(
        verify(&builder.structured(&[write, write], None)),
        Err(VerifyError::SharedNode(write))
    );
}

#[test]
fn a_node_shared_between_the_body_and_the_result_is_rejected() {
    let mut builder = Builder::new();
    let local_id = builder.local(PinpType::Int);
    let [shared] = builder.leaves();
    let write = builder.write(local_id, shared);
    assert_eq!(
        verify(&builder.structured(&[write], Some(shared))),
        Err(VerifyError::SharedNode(shared))
    );
}

#[test]
fn a_node_shared_between_a_statement_and_a_terminator_is_rejected() {
    let mut builder = Builder::new();
    let local_id = builder.local(PinpType::Bool);
    let shared = builder.boolean(true);
    let write = builder.write(local_id, shared);
    let function = builder.flat(vec![
        (
            vec![write],
            Terminator::Branch {
                cond: shared,
                then_block: block(1),
                else_block: block(1),
            },
        ),
        (vec![], Terminator::Return(None)),
    ]);
    assert_eq!(verify(&function), Err(VerifyError::SharedNode(shared)));
}

#[test]
fn a_statement_shared_between_two_blocks_is_rejected() {
    let mut builder = Builder::new();
    let local_id = builder.local(PinpType::Int);
    let [value] = builder.leaves();
    let write = builder.write(local_id, value);
    let function = builder.flat(vec![
        (vec![write], Terminator::Goto(block(1))),
        (vec![write], Terminator::Return(None)),
    ]);
    assert_eq!(verify(&function), Err(VerifyError::SharedNode(write)));
}

#[test]
fn a_statement_shared_between_nested_bodies_is_rejected() {
    let mut builder = Builder::new();
    let local_id = builder.local(PinpType::Int);
    let [value] = builder.leaves();
    let write = builder.write(local_id, value);
    let both = builder.list(&[write]);
    let cond = builder.boolean(true);
    let if_node = builder.statement(Operation::If {
        cond,
        then_body: both,
        else_body: both,
    });
    assert_eq!(
        verify(&builder.structured(&[if_node], None)),
        Err(VerifyError::SharedNode(write))
    );
}

#[test]
fn a_self_referencing_node_is_rejected() {
    let mut builder = Builder::new();
    let itself = builder.arena.operations.next_id();
    let evaluate = builder.evaluate(itself);
    assert_eq!(evaluate, itself);
    assert_eq!(
        verify(&builder.structured(&[evaluate], None)),
        Err(VerifyError::SharedNode(itself))
    );
}

#[test]
fn a_cycle_is_rejected_rather_than_walked_forever() {
    let mut builder = Builder::new();
    let first = builder.arena.operations.next_id();
    let second = NodeId::from_index(first.index() + 1);
    builder.node(
        Operation::Unary {
            op: UnOp::Neg,
            operand: second,
        },
        PinpType::Int,
    );
    builder.node(
        Operation::Unary {
            op: UnOp::Neg,
            operand: first,
        },
        PinpType::Int,
    );
    let evaluate = builder.evaluate(first);
    assert_eq!(
        verify(&builder.structured(&[evaluate], None)),
        Err(VerifyError::SharedNode(first))
    );
}

// ---------------------------------------------------------------------------
// Verifier: statement and value positions
// ---------------------------------------------------------------------------

#[test]
fn a_value_at_the_top_level_is_rejected() {
    let mut builder = Builder::new();
    let [stray] = builder.leaves();
    assert_eq!(
        verify(&builder.structured(&[stray], None)),
        Err(VerifyError::WrongPosition(stray))
    );
}

#[test]
fn a_value_in_a_nested_body_is_rejected() {
    // The stray value sits in a different body of a different construct each time.
    let wrap: [fn(&mut Builder, NodeId) -> NodeId; 5] = [
        |builder, stray| {
            let cond = builder.boolean(true);
            let then_body = builder.list(&[stray]);
            builder.statement(Operation::If {
                cond,
                then_body,
                else_body: List::EMPTY,
            })
        },
        |builder, stray| {
            let cond = builder.boolean(true);
            let else_body = builder.list(&[stray]);
            builder.statement(Operation::If {
                cond,
                then_body: List::EMPTY,
                else_body,
            })
        },
        |builder, stray| {
            let cond = builder.boolean(false);
            let header = builder.list(&[stray]);
            builder.statement(Operation::While {
                header,
                cond,
                body: List::EMPTY,
            })
        },
        |builder, stray| {
            let cond = builder.boolean(false);
            let body = builder.list(&[stray]);
            builder.statement(Operation::DoWhile {
                body,
                cond,
                until: false,
            })
        },
        |builder, stray| {
            let index = builder.local(PinpType::Int);
            let [start, stop] = builder.leaves();
            let range = builder.node(
                Operation::Range {
                    kind: RangeKind::Inclusive,
                    start,
                    stop,
                    step: None,
                },
                PinpType::Range,
            );
            let body = builder.list(&[stray]);
            builder.statement(Operation::CountedLoop { index, range, body })
        },
    ];
    for wrap in wrap {
        let mut builder = Builder::new();
        let local_id = builder.local(PinpType::Int);
        let stray = builder.read(local_id);
        let root = wrap(&mut builder, stray);
        assert_eq!(
            verify(&builder.structured(&[root], None)),
            Err(VerifyError::WrongPosition(stray))
        );
    }
}

#[test]
fn a_value_among_flat_statements_is_rejected() {
    let mut builder = Builder::new();
    let [stray] = builder.leaves();
    let function = builder.flat(vec![(vec![stray], Terminator::Return(None))]);
    assert_eq!(verify(&function), Err(VerifyError::WrongPosition(stray)));
}

#[test]
fn a_statement_as_an_operand_is_rejected() {
    let mut builder = Builder::new();
    let local_id = builder.local(PinpType::Int);
    let [value] = builder.leaves();
    let inner = builder.write(local_id, value);
    let outer = builder.write(local_id, inner);
    assert_eq!(
        verify(&builder.structured(&[outer], None)),
        Err(VerifyError::WrongPosition(inner))
    );
}

#[test]
fn a_statement_as_a_condition_is_rejected() {
    let mut builder = Builder::new();
    let local_id = builder.local(PinpType::Bool);
    let value = builder.boolean(true);
    let write = builder.write(local_id, value);
    let if_node = builder.statement(Operation::If {
        cond: write,
        then_body: List::EMPTY,
        else_body: List::EMPTY,
    });
    assert_eq!(
        verify(&builder.structured(&[if_node], None)),
        Err(VerifyError::WrongPosition(write))
    );
}

#[test]
fn a_statement_in_an_index_list_is_rejected() {
    let mut builder = Builder::new();
    let local_id = builder.local(PinpType::Int);
    let array = builder.local(FLOATS);
    let [value] = builder.leaves();
    let write = builder.write(local_id, value);
    let array_read = builder.read(array);
    let indices = builder.list(&[write]);
    let element = builder.node(
        Operation::ElementGet {
            array: array_read,
            indices,
            checked: true,
        },
        PinpType::Float,
    );
    let evaluate = builder.evaluate(element);
    assert_eq!(
        verify(&builder.structured(&[evaluate], None)),
        Err(VerifyError::WrongPosition(write))
    );
}

#[test]
fn a_statement_as_the_result_is_rejected() {
    let mut builder = Builder::new();
    let local_id = builder.local(PinpType::Int);
    let [value] = builder.leaves();
    let write = builder.write(local_id, value);
    assert_eq!(
        verify(&builder.structured(&[], Some(write))),
        Err(VerifyError::WrongPosition(write))
    );
}

#[test]
fn a_statement_as_a_terminator_operand_is_rejected() {
    let mut builder = Builder::new();
    let local_id = builder.local(PinpType::Int);
    let [value] = builder.leaves();
    let write = builder.write(local_id, value);
    let function = builder.flat(vec![(vec![], Terminator::Return(Some(write)))]);
    assert_eq!(verify(&function), Err(VerifyError::WrongPosition(write)));
}

// ---------------------------------------------------------------------------
// Verifier: levels
// ---------------------------------------------------------------------------

#[test]
fn a_structured_statement_in_a_flat_block_is_rejected() {
    let mut builder = Builder::new();
    let cond = builder.boolean(true);
    let if_node = builder.statement(Operation::If {
        cond,
        then_body: List::EMPTY,
        else_body: List::EMPTY,
    });
    let function = builder.flat(vec![(vec![if_node], Terminator::Return(None))]);
    assert_eq!(
        verify(&function),
        Err(VerifyError::WrongLevel(if_node, Level::Flat))
    );
}

#[test]
fn a_structured_value_deep_in_a_flat_tree_is_rejected() {
    let mut builder = Builder::new();
    let local_id = builder.local(PinpType::Float);
    let array = builder.local(FLOATS);
    let array_read = builder.read(array);
    let [index] = builder.leaves();
    let indices = builder.list(&[index]);
    let element = builder.node(
        Operation::ElementGet {
            array: array_read,
            indices,
            checked: false,
        },
        PinpType::Float,
    );
    let negated = builder.node(
        Operation::Unary {
            op: UnOp::Neg,
            operand: element,
        },
        PinpType::Float,
    );
    let write = builder.write(local_id, negated);
    let function = builder.flat(vec![(vec![write], Terminator::Return(None))]);
    assert_eq!(
        verify(&function),
        Err(VerifyError::WrongLevel(element, Level::Flat))
    );
}

#[test]
fn a_structured_value_as_a_terminator_operand_is_rejected() {
    let mut builder = Builder::new();
    let [value] = builder.leaves();
    let range = builder.node(
        Operation::Range {
            kind: RangeKind::Inclusive,
            start: value,
            stop: value,
            step: None,
        },
        PinpType::Range,
    );
    let function = builder.flat(vec![(vec![], Terminator::Return(Some(range)))]);
    assert_eq!(
        verify(&function),
        Err(VerifyError::WrongLevel(range, Level::Flat))
    );
}

/// Partial lowering: array access and string operations may already be lowered while the control
/// flow is still structured.
///
/// ```text
/// for i in 0..3
///     xs[i] = xs[i]          (as ElementAddress + Load / Store)
///     s.len                  (as a RuntimeCall)
/// ```
#[test]
fn lowered_operations_inside_structured_control_flow_verify() {
    let mut builder = Builder::new();
    let xs = builder.local(FLOATS);
    let i = builder.local(PinpType::Int);
    let text = builder.local(PinpType::Str);
    let mut address_of_element = |builder: &mut Builder| {
        let array = builder.read(xs);
        let index = builder.read(i);
        builder.node(Operation::ElementAddress { array, index }, PinpType::Int)
    };
    let load_address = address_of_element(&mut builder);
    let load = builder.node(
        Operation::Load {
            address: load_address,
            offset: 0,
            stored: ScalarKind::Word,
        },
        PinpType::Float,
    );
    let store_address = address_of_element(&mut builder);
    let store = builder.statement(Operation::Store {
        address: store_address,
        value: load,
        offset: 0,
        stored: ScalarKind::Word,
    });
    let text_read = builder.read(text);
    let args = builder.list(&[text_read]);
    let length = builder.node(
        Operation::RuntimeCall {
            function: RuntimeFunction::StrLength,
            args,
        },
        PinpType::Int,
    );
    let evaluate = builder.evaluate(length);
    let [start, stop] = builder.leaves();
    let range = builder.node(
        Operation::Range {
            kind: RangeKind::UpExclusive,
            start,
            stop,
            step: None,
        },
        PinpType::Range,
    );
    let body = builder.list(&[store, evaluate]);
    let counted_loop = builder.statement(Operation::CountedLoop {
        index: i,
        range,
        body,
    });
    assert_eq!(verify(&builder.structured(&[counted_loop], None)), Ok(()));
}

#[test]
fn a_short_circuit_binary_is_rejected_at_either_level() {
    let build = |builder: &mut Builder| {
        let lhs = builder.boolean(true);
        let rhs = builder.boolean(false);
        builder.node(
            Operation::Binary {
                op: BinOp::And,
                lhs,
                rhs,
            },
            PinpType::Bool,
        )
    };
    let mut builder = Builder::new();
    let and = build(&mut builder);
    assert_eq!(
        verify(&builder.structured(&[], Some(and))),
        Err(VerifyError::WrongLevel(and, Level::Structured))
    );
    let mut builder = Builder::new();
    let and = build(&mut builder);
    let function = builder.flat(vec![(vec![], Terminator::Return(Some(and)))]);
    assert_eq!(
        verify(&function),
        Err(VerifyError::WrongLevel(and, Level::Flat))
    );
}

// ---------------------------------------------------------------------------
// Verifier: dangling ids and missing types
// ---------------------------------------------------------------------------

#[test]
fn a_dangling_statement_is_rejected() {
    let function = Builder::new().structured(&[DANGLING], None);
    assert_eq!(verify(&function), Err(VerifyError::DanglingNode(DANGLING)));
}

#[test]
fn a_dangling_operand_is_rejected() {
    let mut builder = Builder::new();
    let local_id = builder.local(PinpType::Int);
    let write = builder.write(local_id, DANGLING);
    assert_eq!(
        verify(&builder.structured(&[write], None)),
        Err(VerifyError::DanglingNode(DANGLING))
    );
}

#[test]
fn a_dangling_list_element_is_rejected() {
    let mut builder = Builder::new();
    let [first] = builder.leaves();
    let args = builder.list(&[first, DANGLING]);
    let call = builder.node(
        Operation::Call {
            callee: FunctionId::from_index(0),
            args,
        },
        PinpType::Int,
    );
    let evaluate = builder.evaluate(call);
    assert_eq!(
        verify(&builder.structured(&[evaluate], None)),
        Err(VerifyError::DanglingNode(DANGLING))
    );
}

#[test]
fn a_dangling_result_is_rejected() {
    let function = Builder::new().structured(&[], Some(DANGLING));
    assert_eq!(verify(&function), Err(VerifyError::DanglingNode(DANGLING)));
}

#[test]
fn a_dangling_terminator_operand_is_rejected() {
    let function = Builder::new().flat(vec![
        (
            vec![],
            Terminator::Branch {
                cond: DANGLING,
                then_block: block(1),
                else_block: block(1),
            },
        ),
        (vec![], Terminator::Return(None)),
    ]);
    assert_eq!(verify(&function), Err(VerifyError::DanglingNode(DANGLING)));
}

#[test]
fn a_dangling_block_is_rejected() {
    let missing = block(7);
    let terminators = [
        Terminator::Goto(missing),
        Terminator::Branch {
            cond: NodeId::from_index(0),
            then_block: block(0),
            else_block: missing,
        },
        Terminator::Branch {
            cond: NodeId::from_index(0),
            then_block: missing,
            else_block: block(0),
        },
    ];
    for terminator in terminators {
        let mut builder = Builder::new();
        builder.boolean(true);
        let function = builder.flat(vec![(vec![], terminator)]);
        assert_eq!(verify(&function), Err(VerifyError::DanglingBlock(missing)));
    }
}

#[test]
fn a_dangling_entry_is_rejected() {
    let mut function = Builder::new().flat(vec![(vec![], Terminator::Return(None))]);
    function.shape.entry = block(1);
    assert_eq!(verify(&function), Err(VerifyError::DanglingBlock(block(1))));
}

#[test]
fn a_dangling_local_is_rejected() {
    let missing = LocalId::from_index(42);
    let int_id = TypeTable::default().intern(PinpType::Int);
    let operations: [fn(LocalId, NodeId) -> Operation; 3] = [
        |local_id, _| Operation::ReadLocal(local_id),
        |local_id, value| Operation::WriteLocal { local_id, value },
        |local_id, value| Operation::CountedLoop {
            index: local_id,
            range: value,
            body: List::EMPTY,
        },
    ];
    for operation in operations {
        let mut builder = Builder::new();
        let [value] = builder.leaves();
        let node_id = builder.arena.push(operation(missing, value), int_id, SPAN);
        let root = match builder.arena.operations[node_id].is_statement() {
            true => node_id,
            false => builder.evaluate(node_id),
        };
        assert_eq!(
            verify(&builder.structured(&[root], None)),
            Err(VerifyError::DanglingLocal(node_id))
        );
    }
}

/// The node has neither a type nor a span; the missing type is what gets reported.
#[test]
fn an_untyped_node_is_rejected() {
    let mut builder = Builder::new();
    let local_id = builder.local(PinpType::Int);
    let untyped = builder
        .arena
        .operations
        .push(Operation::ReadGlobal(GlobalId::from_index(0)));
    let write = builder.write(local_id, untyped);
    assert_eq!(
        verify(&builder.structured(&[write], None)),
        Err(VerifyError::Untyped(untyped))
    );
}

#[test]
fn an_untyped_statement_is_rejected() {
    let mut builder = Builder::new();
    let [value] = builder.leaves();
    let untyped = builder.arena.operations.push(Operation::Evaluate(value));
    assert_eq!(
        verify(&builder.structured(&[untyped], None)),
        Err(VerifyError::Untyped(untyped))
    );
}

// ---------------------------------------------------------------------------
// Verifier: every node has a span
// ---------------------------------------------------------------------------

impl Builder {
    /// A typed node whose span was never set — what a lowering that forgets the origin produces.
    fn unspanned(&mut self, operation: Operation, pinp_type: PinpType) -> NodeId {
        let type_id = self.tables.types.intern(pinp_type);
        self.arena.push(operation, type_id, Span::UNKNOWN)
    }
}

#[test]
fn an_unspanned_operand_is_rejected() {
    let mut builder = Builder::new();
    let local_id = builder.local(PinpType::Int);
    let constant_id = builder.tables.constants.intern(Constant::Int(3));
    let unspanned = builder.unspanned(Operation::Constant(constant_id), PinpType::Int);
    let write = builder.write(local_id, unspanned);
    assert_eq!(
        verify(&builder.structured(&[write], None)),
        Err(VerifyError::Unspanned(unspanned))
    );
}

#[test]
fn an_unspanned_statement_is_rejected() {
    let mut builder = Builder::new();
    let [value] = builder.leaves();
    let unspanned = builder.unspanned(Operation::Evaluate(value), PinpType::Void);
    assert_eq!(
        verify(&builder.structured(&[unspanned], None)),
        Err(VerifyError::Unspanned(unspanned))
    );
}

#[test]
fn an_unspanned_node_in_a_nested_body_is_rejected() {
    let mut builder = Builder::new();
    let [value] = builder.leaves();
    let unspanned = builder.unspanned(Operation::Evaluate(value), PinpType::Void);
    let cond = builder.boolean(true);
    let else_body = builder.list(&[unspanned]);
    let if_node = builder.statement(Operation::If {
        cond,
        then_body: List::EMPTY,
        else_body,
    });
    assert_eq!(
        verify(&builder.structured(&[if_node], None)),
        Err(VerifyError::Unspanned(unspanned))
    );
}

#[test]
fn an_unspanned_result_or_terminator_operand_is_rejected() {
    let mut builder = Builder::new();
    let constant_id = builder.tables.constants.intern(Constant::Int(3));
    let unspanned = builder.unspanned(Operation::Constant(constant_id), PinpType::Int);
    assert_eq!(
        verify(&builder.structured(&[], Some(unspanned))),
        Err(VerifyError::Unspanned(unspanned))
    );

    let mut builder = Builder::new();
    let constant_id = builder.tables.constants.intern(Constant::Int(3));
    let unspanned = builder.unspanned(Operation::Constant(constant_id), PinpType::Int);
    let function = builder.flat(vec![(vec![], Terminator::Return(Some(unspanned)))]);
    assert_eq!(verify(&function), Err(VerifyError::Unspanned(unspanned)));
}

// ---------------------------------------------------------------------------
// Flat graph
// ---------------------------------------------------------------------------

#[test]
fn the_diamond_has_the_expected_edges() {
    let function = diamond_example();
    let flat = &function.shape;
    assert_eq!(flat.successors(block(0)), vec![block(1), block(2)]);
    assert_eq!(flat.successors(block(1)), vec![block(3)]);
    assert_eq!(flat.successors(block(2)), vec![block(3)]);
    assert!(flat.successors(block(3)).is_empty());
    let predecessors = flat.predecessors();
    assert!(predecessors[block(0)].is_empty());
    assert_eq!(predecessors[block(1)], vec![block(0)]);
    assert_eq!(predecessors[block(2)], vec![block(0)]);
    assert_eq!(predecessors[block(3)], vec![block(1), block(2)]);
}

#[test]
fn a_back_edge_makes_the_loop_header_its_own_predecessor() {
    let mut builder = Builder::new();
    let cond = builder.boolean(true);
    // block0 → block1; block1 loops to itself or leaves to block2.
    let function = builder.flat(vec![
        (vec![], Terminator::Goto(block(1))),
        (
            vec![],
            Terminator::Branch {
                cond,
                then_block: block(1),
                else_block: block(2),
            },
        ),
        (vec![], Terminator::Return(None)),
    ]);
    assert_eq!(verify(&function), Ok(()));
    let predecessors = function.shape.predecessors();
    assert_eq!(predecessors[block(1)], vec![block(0), block(1)]);
    assert_eq!(predecessors[block(2)], vec![block(1)]);
}

#[test]
fn an_error_exit_has_no_successors() {
    let mut builder = Builder::new();
    let cond = builder.boolean(false);
    let message = builder.tables.constants.intern(Constant::Str(StrId(0)));
    let function = builder.flat(vec![
        (
            vec![],
            Terminator::Branch {
                cond,
                then_block: block(1),
                else_block: block(2),
            },
        ),
        (vec![], Terminator::Return(None)),
        (vec![], Terminator::RuntimeError(message)),
    ]);
    assert_eq!(verify(&function), Ok(()));
    assert!(function.shape.successors(block(2)).is_empty());
    assert_eq!(function.shape.predecessors()[block(2)], vec![block(0)]);
}

#[test]
fn an_unreachable_block_still_counts_as_a_predecessor() {
    // block1 is unreachable, yet its edge into block2 is real: reachability is an analysis, not
    // a property of the graph.
    let function = Builder::new().flat(vec![
        (vec![], Terminator::Goto(block(2))),
        (vec![], Terminator::Goto(block(2))),
        (vec![], Terminator::Return(None)),
    ]);
    assert_eq!(verify(&function), Ok(()));
    let predecessors = function.shape.predecessors();
    assert!(predecessors[block(1)].is_empty());
    assert_eq!(predecessors[block(2)], vec![block(0), block(1)]);
}

#[test]
fn a_branch_with_identical_targets_counts_both_edges() {
    let mut builder = Builder::new();
    let cond = builder.boolean(true);
    let function = builder.flat(vec![
        (
            vec![],
            Terminator::Branch {
                cond,
                then_block: block(1),
                else_block: block(1),
            },
        ),
        (vec![], Terminator::Return(None)),
    ]);
    assert_eq!(
        function.shape.successors(block(0)),
        vec![block(1), block(1)]
    );
    assert_eq!(
        function.shape.predecessors()[block(1)],
        vec![block(0), block(0)]
    );
}

// ---------------------------------------------------------------------------
// Switch (the flat form of a `match` on an ADT tag)
// ---------------------------------------------------------------------------

impl Builder {
    fn switch(&mut self, tag: NodeId, targets: &[BlockId], default: BlockId) -> Terminator {
        let targets = self.arena.list(targets);
        Terminator::Switch {
            tag,
            targets,
            default,
        }
    }
}

/// block0 switches on a tag into block2, block1, or (by default) block3; each returns.
fn switch_example(targets: &[BlockId], default: BlockId) -> Function<Flat> {
    let mut builder = Builder::new();
    let tag = builder.int(0);
    let switch = builder.switch(tag, targets, default);
    builder.flat(vec![
        (vec![], switch),
        (vec![], Terminator::Return(None)),
        (vec![], Terminator::Return(None)),
        (vec![], Terminator::Return(None)),
    ])
}

#[test]
fn a_valid_switch_verifies() {
    assert_eq!(
        verify(&switch_example(&[block(2), block(1)], block(3))),
        Ok(())
    );
}

#[test]
fn a_switch_leads_to_its_targets_in_order_then_its_default() {
    let function = switch_example(&[block(2), block(1)], block(3));
    let flat = &function.shape;
    assert_eq!(
        flat.successors(block(0)),
        vec![block(2), block(1), block(3)]
    );
    let predecessors = flat.predecessors();
    for target in [block(1), block(2), block(3)] {
        assert_eq!(predecessors[target], vec![block(0)]);
    }
}

#[test]
fn a_switch_without_targets_leads_only_to_its_default() {
    let function = switch_example(&[], block(3));
    assert_eq!(verify(&function), Ok(()));
    assert_eq!(function.shape.successors(block(0)), vec![block(3)]);
}

#[test]
fn a_switch_repeating_a_block_counts_every_edge() {
    let function = switch_example(&[block(1), block(1)], block(1));
    assert_eq!(verify(&function), Ok(()));
    assert_eq!(
        function.shape.successors(block(0)),
        vec![block(1), block(1), block(1)]
    );
    assert_eq!(
        function.shape.predecessors()[block(1)],
        vec![block(0), block(0), block(0)]
    );
}

#[test]
fn a_dangling_switch_target_or_default_is_rejected() {
    let missing = block(9);
    assert_eq!(
        verify(&switch_example(&[block(1), missing], block(3))),
        Err(VerifyError::DanglingBlock(missing))
    );
    assert_eq!(
        verify(&switch_example(&[block(1)], missing)),
        Err(VerifyError::DanglingBlock(missing))
    );
}

#[test]
fn a_dangling_switch_tag_is_rejected() {
    let mut builder = Builder::new();
    let switch = builder.switch(DANGLING, &[], block(1));
    let function = builder.flat(vec![(vec![], switch), (vec![], Terminator::Return(None))]);
    assert_eq!(verify(&function), Err(VerifyError::DanglingNode(DANGLING)));
}

#[test]
fn a_statement_as_a_switch_tag_is_rejected() {
    let mut builder = Builder::new();
    let local_id = builder.local(PinpType::Int);
    let [value] = builder.leaves();
    let write = builder.write(local_id, value);
    let switch = builder.switch(write, &[], block(1));
    let function = builder.flat(vec![(vec![], switch), (vec![], Terminator::Return(None))]);
    assert_eq!(verify(&function), Err(VerifyError::WrongPosition(write)));
}

#[test]
fn a_switch_tag_shared_with_a_statement_is_rejected() {
    let mut builder = Builder::new();
    let local_id = builder.local(PinpType::Int);
    let [shared] = builder.leaves();
    let write = builder.write(local_id, shared);
    let switch = builder.switch(shared, &[], block(1));
    let function = builder.flat(vec![
        (vec![write], switch),
        (vec![], Terminator::Return(None)),
    ]);
    assert_eq!(verify(&function), Err(VerifyError::SharedNode(shared)));
}

#[test]
fn a_structured_value_as_a_switch_tag_is_rejected() {
    let mut builder = Builder::new();
    let [start, stop] = builder.leaves();
    let range = builder.node(
        Operation::Range {
            kind: RangeKind::Inclusive,
            start,
            stop,
            step: None,
        },
        PinpType::Range,
    );
    let switch = builder.switch(range, &[], block(1));
    let function = builder.flat(vec![(vec![], switch), (vec![], Terminator::Return(None))]);
    assert_eq!(
        verify(&function),
        Err(VerifyError::WrongLevel(range, Level::Flat))
    );
}

// ---------------------------------------------------------------------------
// The loop example, end to end
// ---------------------------------------------------------------------------

#[test]
fn the_loop_example_reads_what_the_source_reads() {
    let function = loop_example();
    let mut found = Vec::new();
    function.shape.for_each_root(&function.arena, |root, _| {
        function
            .arena
            .for_each_read(root, |local_id| found.push(local_id.index()))
    });
    found.sort();
    // n (range stop), total twice (update, result), xs, i.
    assert_eq!(
        found,
        vec![0, 1, 1, 2, 3],
        "{}",
        indoc! {"
        n is read by the range, total by the update and the result, xs and i by the access.
    "}
    );
}
