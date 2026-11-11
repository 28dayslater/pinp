// SPDX-License-Identifier: MIT

//! Flattening: a structured function rewritten as its control-flow graph — the flat level.
//!
//! Nested control flow (`If`, `While`, `DoWhile`, `CountedLoop`) becomes basic blocks joined by
//! branches and gotos, and a `Return` the terminator of its block; every other statement moves
//! into a block unchanged. The function keeps its arena, so every node keeps its id, type, and
//! span, and the side tables indexed by them stay valid. Runtime checks stay inside their
//! expressions: the backend splits blocks at them.
//!
//! Blocks are numbered as they are met: an `If` creates its then, else, and join blocks, and a
//! loop its header, body, and exit blocks, before what is inside them is flattened.

use crate::ir::{
    Arena, BasicBlock, BlockId, Constant, Flat, Function, IdVec, List, Local, LocalId, NodeId,
    Operation, Program, ProgramTables, RangeFieldKind, SideTable, Structured, Terminator,
};
use crate::lexer::Span;
use crate::parser::{BinOp, PinpType, RangeKind, UnOp};

#[cfg(test)]
mod tests;

/// Flattens every function of `program`, keeping their ids and the entry.
pub fn flatten_program(program: Program<Structured>) -> Program<Flat> {
    let Program {
        mut tables,
        functions,
        entry,
    } = program;
    let mut flat_functions = IdVec::with_capacity(functions.len());
    for function in functions.into_items() {
        flat_functions.push(flatten(function, &mut tables));
    }
    Program {
        tables,
        functions: flat_functions,
        entry,
    }
}

/// Rewrites `function` as blocks and branches, over the same arena.
pub fn flatten(function: Function<Structured>, tables: &mut ProgramTables) -> Function<Flat> {
    let Function {
        id,
        parameter_count,
        arena,
        shape,
    } = function;
    let mut flattener = Flattener {
        arena,
        tables,
        statements: Vec::new(),
        terminators: Vec::new(),
        current: BlockId::from_index(0),
        reachable: true,
        counted_loop_ranges: SideTable::default(),
    };
    let entry = flattener.new_block();
    flattener.current = entry;
    flattener.statements_of(shape.body);
    let end = match (shape.result, flattener.reachable) {
        (None, false) => Terminator::Unreachable,
        (result, _) => Terminator::Return(result),
    };
    flattener.terminate(end);

    let Flattener {
        mut arena,
        statements,
        terminators,
        counted_loop_ranges,
        ..
    } = flattener;
    let mut blocks = IdVec::default();
    for (statements, terminator) in statements.into_iter().zip(terminators) {
        blocks.push(BasicBlock {
            statements: arena.list(&statements),
            terminator: terminator.expect("flattening terminates every block it creates"),
        });
    }
    let flat = Function {
        id,
        parameter_count,
        arena,
        shape: Flat {
            blocks,
            entry,
            counted_loop_ranges,
        },
    };
    debug_assert_eq!(crate::ir::verify(&flat), Ok(()), "flattening broke the IR");
    flat
}

/// What is known, before running, about how a counted loop's range runs.
#[derive(Clone, Copy)]
struct KnownRange {
    /// `Some(true)` when the range counts up, `Some(false)` down; `None` when only its step's sign
    /// at run time says.
    upward: Option<bool>,
    /// Whether the stop is included, when known.
    inclusive: Option<bool>,
}

struct Flattener<'tables> {
    arena: Arena,
    tables: &'tables mut ProgramTables,
    /// Per block, in id order.
    statements: Vec<Vec<NodeId>>,
    terminators: Vec<Option<Terminator>>,
    current: BlockId,
    /// Whether control can reach the current block, as far as the structure says — no condition
    /// is evaluated. Code the compiler generates is not placed where it cannot run.
    reachable: bool,
    counted_loop_ranges: SideTable<BlockId, Option<NodeId>>,
}

impl Flattener<'_> {
    fn new_block(&mut self) -> BlockId {
        let block_id = BlockId::from_index(self.statements.len() as u32);
        self.statements.push(Vec::new());
        self.terminators.push(None);
        block_id
    }

    fn append(&mut self, statement: NodeId) {
        self.statements[self.current.index() as usize].push(statement);
    }

    fn terminate(&mut self, terminator: Terminator) {
        self.terminators[self.current.index() as usize] = Some(terminator);
    }

    fn statements_of(&mut self, body: List<NodeId>) {
        for statement in self.arena.items(body).to_vec() {
            self.statement(statement);
        }
    }

    fn statement(&mut self, statement: NodeId) {
        let reachable = self.reachable;
        match self.arena.operations[statement] {
            Operation::Return(value) => {
                self.terminate(Terminator::Return(value));
                // What follows lands in a block nothing enters.
                self.current = self.new_block();
                self.reachable = false;
            }
            Operation::If {
                cond,
                then_body,
                else_body,
            } => {
                let then_block = self.new_block();
                let else_block = (!else_body.is_empty()).then(|| self.new_block());
                let join = self.new_block();
                self.terminate(Terminator::Branch {
                    cond,
                    then_block,
                    else_block: else_block.unwrap_or(join),
                });
                self.current = then_block;
                self.statements_of(then_body);
                self.terminate(Terminator::Goto(join));
                let mut join_reachable = self.reachable;
                self.reachable = reachable;
                if let Some(else_block) = else_block {
                    self.current = else_block;
                    self.statements_of(else_body);
                    self.terminate(Terminator::Goto(join));
                }
                join_reachable |= self.reachable;
                self.current = join;
                self.reachable = join_reachable;
            }
            Operation::While { header, cond, body } => {
                let (header_block, body_block, exit) =
                    (self.new_block(), self.new_block(), self.new_block());
                self.terminate(Terminator::Goto(header_block));
                self.current = header_block;
                self.statements_of(header);
                self.terminate(Terminator::Branch {
                    cond,
                    then_block: body_block,
                    else_block: exit,
                });
                self.current = body_block;
                self.statements_of(body);
                self.terminate(Terminator::Goto(header_block));
                self.current = exit;
                self.reachable = reachable;
            }
            Operation::DoWhile { body, cond, until } => {
                let (body_block, exit) = (self.new_block(), self.new_block());
                self.terminate(Terminator::Goto(body_block));
                self.current = body_block;
                self.statements_of(body);
                let (then_block, else_block) = match until {
                    true => (exit, body_block),
                    false => (body_block, exit),
                };
                self.terminate(Terminator::Branch {
                    cond,
                    then_block,
                    else_block,
                });
                // The exit is reached only from the end of the body, so it inherits its flag.
                self.current = exit;
            }
            Operation::CountedLoop { index, range, body } => {
                self.counted_loop(statement, index, range, body)
            }
            _ => self.append(statement),
        }
    }

    /// `for index in range`: the range saved once; the index starting at its start; a header
    /// testing the index against the stop; the body ending by adding the step.
    // TODO(KI-0007): a step past an integer limit wraps, and the loop never ends.
    fn counted_loop(
        &mut self,
        statement: NodeId,
        index: LocalId,
        range: NodeId,
        body: List<NodeId>,
    ) {
        let span = self.arena.spans[statement];
        let known = self.known_range(range);
        let range_type = self.type_id(PinpType::Range);
        let saved_range = self.arena.locals.push(Local {
            name: None,
            type_id: range_type,
        });
        self.append_write(saved_range, range, span);
        let start = self.field(saved_range, RangeFieldKind::Start, span);
        self.append_write(index, start, span);

        let (header, body_block, exit) = (self.new_block(), self.new_block(), self.new_block());
        self.counted_loop_ranges[header] = Some(range);
        self.terminate(Terminator::Goto(header));
        self.current = header;
        let cond = self.loop_test(index, saved_range, known, span);
        self.terminate(Terminator::Branch {
            cond,
            then_block: body_block,
            else_block: exit,
        });
        let reachable = self.reachable;
        self.current = body_block;
        self.statements_of(body);
        // A body that always returns never gets to its step.
        if self.reachable {
            let current = self.read_local(index, span);
            let step = self.field(saved_range, RangeFieldKind::Step, span);
            let next = self.push(
                Operation::Binary {
                    op: BinOp::Add,
                    lhs: current,
                    rhs: step,
                },
                PinpType::Int,
                span,
            );
            self.append_write(index, next, span);
        }
        self.terminate(Terminator::Goto(header));
        self.current = exit;
        self.reachable = reachable;
    }

    /// What the range's own node says: a range written in place has a known kind, and a known
    /// direction when its step is a constant — or, without a step, when it is exclusive. A written
    /// step decides the direction whatever the spelling: `5..>1:2` counts up. A range read from a
    /// variable says nothing until run time.
    fn known_range(&self, range: NodeId) -> KnownRange {
        let Operation::Range { kind, step, .. } = self.arena.operations[range] else {
            return KnownRange {
                upward: None,
                inclusive: None,
            };
        };
        let upward = match (kind, step) {
            (_, Some(step)) => self.constant_sign(step),
            (RangeKind::UpExclusive, None) => Some(true),
            (RangeKind::DownExclusive, None) => Some(false),
            (RangeKind::Inclusive, None) => None,
        };
        KnownRange {
            upward,
            inclusive: Some(kind == RangeKind::Inclusive),
        }
    }

    /// Whether a step node is a positive constant (`Some(true)`) or a negative one, written as a
    /// negated constant (`Some(false)`). A zero step stops the program before the loop runs.
    fn constant_sign(&self, step: NodeId) -> Option<bool> {
        let (operand, negated) = match self.arena.operations[step] {
            Operation::Unary {
                op: UnOp::Neg,
                operand,
            } => (operand, true),
            _ => (step, false),
        };
        let Operation::Constant(constant_id) = self.arena.operations[operand] else {
            return None;
        };
        match self.tables.constants.get(constant_id) {
            Constant::Int(value) if value != 0 => Some((value > 0) != negated),
            _ => None,
        }
    }

    /// Whether the index is still within the range: `index <= stop` counting up inclusively,
    /// `index < stop` exclusively, and the mirror counting down — each decided at run time only
    /// when not known now.
    fn loop_test(
        &mut self,
        index: LocalId,
        range: LocalId,
        known: KnownRange,
        span: Span,
    ) -> NodeId {
        let upward_test = (known.upward != Some(false))
            .then(|| self.bound_test(index, range, known.inclusive, BinOp::Le, BinOp::Lt, span));
        let downward_test = (known.upward != Some(true))
            .then(|| self.bound_test(index, range, known.inclusive, BinOp::Ge, BinOp::Gt, span));
        match (upward_test, downward_test) {
            (Some(test), None) | (None, Some(test)) => test,
            (Some(upward_test), Some(downward_test)) => {
                let step = self.field(range, RangeFieldKind::Step, span);
                let zero = self.int_constant(0, span);
                let upward = self.compare(BinOp::Gt, step, zero, span);
                self.select(upward, upward_test, downward_test, span)
            }
            (None, None) => unreachable!("a range runs in at least one direction"),
        }
    }

    /// `index inclusive_op stop` or `index exclusive_op stop`, chosen now if the range's kind is
    /// known and at run time otherwise.
    fn bound_test(
        &mut self,
        index: LocalId,
        range: LocalId,
        inclusive: Option<bool>,
        inclusive_op: BinOp,
        exclusive_op: BinOp,
        span: Span,
    ) -> NodeId {
        let test = |flattener: &mut Self, op| {
            let current = flattener.read_local(index, span);
            let stop = flattener.field(range, RangeFieldKind::Stop, span);
            flattener.compare(op, current, stop, span)
        };
        match inclusive {
            Some(true) => test(self, inclusive_op),
            Some(false) => test(self, exclusive_op),
            None => {
                let included = self.field(range, RangeFieldKind::Inclusive, span);
                let inclusive_test = test(self, inclusive_op);
                let exclusive_test = test(self, exclusive_op);
                self.select(included, inclusive_test, exclusive_test, span)
            }
        }
    }

    fn field(&mut self, range: LocalId, field: RangeFieldKind, span: Span) -> NodeId {
        let range = self.read_local(range, span);
        let field_type = match field {
            RangeFieldKind::Inclusive => PinpType::Bool,
            _ => PinpType::Int,
        };
        self.push(Operation::RangeField { range, field }, field_type, span)
    }

    fn compare(&mut self, op: BinOp, lhs: NodeId, rhs: NodeId, span: Span) -> NodeId {
        self.push(Operation::Binary { op, lhs, rhs }, PinpType::Bool, span)
    }

    fn select(&mut self, cond: NodeId, if_true: NodeId, if_false: NodeId, span: Span) -> NodeId {
        let operation = Operation::Select {
            cond,
            if_true,
            if_false,
        };
        self.push(operation, PinpType::Bool, span)
    }

    fn int_constant(&mut self, value: i64, span: Span) -> NodeId {
        let constant_id = self.tables.constants.intern(Constant::Int(value));
        self.push(Operation::Constant(constant_id), PinpType::Int, span)
    }

    fn read_local(&mut self, local_id: LocalId, span: Span) -> NodeId {
        let type_id = self.arena.locals[local_id].type_id;
        self.arena
            .push(Operation::ReadLocal(local_id), type_id, span)
    }

    fn append_write(&mut self, local_id: LocalId, value: NodeId, span: Span) {
        let write = self.push(
            Operation::WriteLocal { local_id, value },
            PinpType::Void,
            span,
        );
        self.append(write);
    }

    fn push(&mut self, operation: Operation, pinp_type: PinpType, span: Span) -> NodeId {
        let type_id = self.type_id(pinp_type);
        self.arena.push(operation, type_id, span)
    }

    fn type_id(&mut self, pinp_type: PinpType) -> crate::ir::TypeId {
        self.tables.types.intern(pinp_type)
    }
}
