// SPDX-License-Identifier: MIT

//! [`Printer`]: the IR as readable text, one line per node, children indented under their parent.
//!
//! Used by golden tests and for investigating a [`verify`](super::verify) failure, so broken IR
//! prints rather than panics: a dangling id as `<dangling n7>`, a node's second appearance as
//! `<shared n7>` (which also stops a cycle), and a missing name, local, or type as `?`.

use std::fmt::Write;

use super::{
    Arena, Constant, Flat, Function, FunctionId, GlobalId, List, LocalId, NodeId, Operation,
    ProgramTables, RangeFieldKind, ScalarKind, Shape, SideTable, Structured, Terminator,
};
use crate::parser::{ArrayElementType, BinOp, PinpType, RangeKind, SymId, UnOp};

/// Prints IR, naming locals and string constants from the AST's tables.
pub struct Printer<'a> {
    tables: &'a ProgramTables,
    names: &'a [&'a str],
    strings: &'a [String],
}

/// A shape the printer knows how to lay out.
pub trait PrintableShape: Shape {
    /// The name the header uses for the level.
    const LEVEL_NAME: &'static str;

    fn print_sections(&self, writer: &mut Writer);
}

impl<'a> Printer<'a> {
    /// `names` are the AST's symbol names, `strings` its string literals.
    pub fn new(tables: &'a ProgramTables, names: &'a [&'a str], strings: &'a [String]) -> Self {
        Printer {
            tables,
            names,
            strings,
        }
    }

    pub fn function<S: PrintableShape>(&self, function: &Function<S>) -> String {
        let mut writer = Writer::new(self, &function.arena);
        let count = function.parameter_count;
        let plural = if count == 1 { "" } else { "s" };
        writer.line(
            0,
            format!(
                "function {} ({count} parameter{plural}), {}",
                self.function_name(function.id),
                S::LEVEL_NAME
            ),
        );
        if !function.arena.locals.is_empty() {
            writer.line(0, "locals".to_string());
            for (local_id, local) in function.arena.locals.iter() {
                let pinp_type = self.tables.types.get(local.type_id);
                let declared = format!("{}: {}", writer.local(local_id), type_name(pinp_type));
                writer.line(1, declared);
            }
        }
        function.shape.print_sections(&mut writer);
        writer.out
    }

    /// `f2 double`; just `f2` for a function the tables do not name.
    fn function_name(&self, function_id: FunctionId) -> String {
        let signature = self.tables.signatures.get(function_id);
        match signature.and_then(|signature| signature.name) {
            Some(name) => format!("f{} {}", function_id.index(), self.name(name)),
            None => format!("f{}", function_id.index()),
        }
    }

    /// `g0 total`; just `g0` for a global the tables do not have.
    fn global_name(&self, global_id: GlobalId) -> String {
        match self.tables.globals.get(global_id) {
            Some(global) => format!("g{} {}", global_id.index(), self.name(global.name)),
            None => format!("g{}", global_id.index()),
        }
    }

    fn name(&self, name: SymId) -> &str {
        self.names.get(name.value()).copied().unwrap_or("?")
    }

    /// The tree under `root`.
    pub fn tree(&self, arena: &Arena, root: NodeId) -> String {
        let mut writer = Writer::new(self, arena);
        writer.tree(root, 0);
        writer.out
    }
}

/// One printing run: where the text goes, and which nodes have been printed already.
pub struct Writer<'a> {
    printer: &'a Printer<'a>,
    arena: &'a Arena,
    out: String,
    seen: SideTable<NodeId, bool>,
}

/// What a line of a tree shows: a node, or the label of one of its bodies.
enum Entry {
    Node(NodeId),
    Label(&'static str),
}

impl<'a> Writer<'a> {
    fn new(printer: &'a Printer<'a>, arena: &'a Arena) -> Self {
        Writer {
            printer,
            arena,
            out: String::new(),
            seen: SideTable::default(),
        }
    }

    fn line(&mut self, depth: usize, text: String) {
        let _ = writeln!(self.out, "{:indent$}{text}", "", indent = depth * 2);
    }

    /// Prints the tree under `root`, iteratively so that a deep tree cannot overflow the stack.
    fn tree(&mut self, root: NodeId, depth: usize) {
        let mut pending = vec![(Entry::Node(root), depth)];
        while let Some((entry, depth)) = pending.pop() {
            let node_id = match entry {
                Entry::Label(label) => {
                    self.line(depth, label.to_string());
                    continue;
                }
                Entry::Node(node_id) => node_id,
            };
            let Some(&operation) = self.arena.operations.get(node_id) else {
                self.line(depth, format!("<dangling n{}>", node_id.index()));
                continue;
            };
            if self.seen[node_id] {
                self.line(depth, format!("<shared n{}>", node_id.index()));
                continue;
            }
            self.seen[node_id] = true;
            let text = format!("{}{}", self.describe(operation), self.type_suffix(node_id));
            self.line(depth, text);
            let first_child = pending.len();
            self.for_each_entry(operation, node_id, |entry, offset| {
                pending.push((entry, depth + offset))
            });
            pending[first_child..].reverse();
        }
    }

    /// A node's children in order, with the labels of its bodies, each with its extra depth.
    fn for_each_entry(
        &self,
        operation: Operation,
        node_id: NodeId,
        mut visit: impl FnMut(Entry, usize),
    ) {
        let body = |label, list: List<NodeId>, visit: &mut dyn FnMut(Entry, usize)| {
            visit(Entry::Label(label), 1);
            for &statement in self.arena.items(list) {
                visit(Entry::Node(statement), 2);
            }
        };
        match operation {
            Operation::If {
                cond,
                then_body,
                else_body,
            } => {
                visit(Entry::Node(cond), 1);
                body("then", then_body, &mut visit);
                if !else_body.is_empty() {
                    body("else", else_body, &mut visit);
                }
            }
            Operation::While {
                header,
                cond,
                body: loop_body,
            } => {
                if !header.is_empty() {
                    body("header", header, &mut visit);
                }
                visit(Entry::Node(cond), 1);
                body("body", loop_body, &mut visit);
            }
            Operation::DoWhile {
                body: loop_body,
                cond,
                ..
            } => {
                body("body", loop_body, &mut visit);
                visit(Entry::Node(cond), 1);
            }
            Operation::CountedLoop {
                range,
                body: loop_body,
                ..
            } => {
                visit(Entry::Node(range), 1);
                body("body", loop_body, &mut visit);
            }
            _ => self
                .arena
                .for_each_child(node_id, |child| visit(Entry::Node(child), 1)),
        }
    }

    fn type_suffix(&self, node_id: NodeId) -> String {
        match self.arena.types[node_id] {
            None => ": ?".to_string(),
            Some(type_id) => match self.printer.tables.types.get(type_id) {
                PinpType::Void => String::new(),
                pinp_type => format!(": {}", type_name(pinp_type)),
            },
        }
    }

    /// The operation's name, then its flags and the ids it names.
    fn describe(&self, operation: Operation) -> String {
        use Operation::*;
        let checked = |name: &str, checked: bool| match checked {
            true => format!("{name} checked"),
            false => name.to_string(),
        };
        match operation {
            Constant(constant_id) => {
                let constant = self.printer.tables.constants.get(constant_id);
                format!("constant {}", self.constant(constant))
            }
            ReadLocal(local_id) => format!("read_local {}", self.local(local_id)),
            ReadGlobal(global_id) => format!("read_global {}", self.printer.global_name(global_id)),
            WriteLocal { local_id, .. } => format!("write_local {}", self.local(local_id)),
            WriteGlobal { global_id, .. } => {
                format!("write_global {}", self.printer.global_name(global_id))
            }
            Evaluate(_) => "evaluate".to_string(),
            Unary { op, .. } => match op {
                UnOp::Neg => "negate",
                UnOp::Not => "not",
            }
            .to_string(),
            Binary { op, .. } => binary_word(op).to_string(),
            IntegerDivide {
                remainder,
                checked: is_checked,
                ..
            } => checked(if remainder { "mod" } else { "div" }, is_checked),
            Convert { .. } => "convert".to_string(),
            Select { .. } => "select".to_string(),
            RangeField { field, .. } => format!(
                "range_field {}",
                match field {
                    RangeFieldKind::Start => "start",
                    RangeFieldKind::Stop => "stop",
                    RangeFieldKind::Step => "step",
                    RangeFieldKind::Inclusive => "inclusive",
                }
            ),
            Call { callee, .. } => format!("call {}", self.printer.function_name(callee)),
            RuntimeCall { function, .. } => format!("runtime_call {}", function.name()),
            ElementAddress { .. } => "element_address".to_string(),
            Load { offset, stored, .. } => format!("load {} +{offset}", scalar_name(stored)),
            Store { offset, stored, .. } => format!("store {} +{offset}", scalar_name(stored)),
            Range { kind, .. } => format!(
                "range {}",
                match kind {
                    RangeKind::Inclusive => "inclusive",
                    RangeKind::UpExclusive => "up_exclusive",
                    RangeKind::DownExclusive => "down_exclusive",
                }
            ),
            Membership { .. } => "in".to_string(),
            ArrayNew { .. } => "array_new".to_string(),
            ArrayAllocate { zeroed } => match zeroed {
                true => "array_allocate zeroed".to_string(),
                false => "array_allocate".to_string(),
            },
            ElementGet {
                checked: is_checked,
                ..
            } => checked("element_get", is_checked),
            ElementSet {
                checked: is_checked,
                ..
            } => checked("element_set", is_checked),
            Slice { .. } => "slice".to_string(),
            If { .. } => "if".to_string(),
            While { .. } => "while".to_string(),
            DoWhile { until, .. } => match until {
                true => "do_until".to_string(),
                false => "do_while".to_string(),
            },
            CountedLoop { index, .. } => format!("counted_loop {}", self.local(index)),
            Return(_) => "return".to_string(),
            Break(depth) => format!("break {depth}"),
            Continue(depth) => format!("continue {depth}"),
        }
    }

    /// `l3 total`; a temporary is just `l3`, and an unknown local or name is `?`.
    fn local(&self, local_id: LocalId) -> String {
        let name = match self.arena.locals.get(local_id) {
            None => Some("?"),
            Some(local) => local
                .name
                .map(|name| self.printer.names.get(name.value()).copied().unwrap_or("?")),
        };
        match name {
            Some(name) => format!("l{} {name}", local_id.index()),
            None => format!("l{}", local_id.index()),
        }
    }

    /// A constant as pinp source would write it.
    fn constant(&self, constant: Constant) -> String {
        match constant {
            Constant::Bool(value) => value.to_string(),
            Constant::Int(value) => value.to_string(),
            Constant::Float(bits) => {
                let value = f64::from_bits(bits);
                match value {
                    _ if value.is_nan() => "nan".to_string(),
                    f64::INFINITY => "inf".to_string(),
                    f64::NEG_INFINITY => "-inf".to_string(),
                    _ => ryu::Buffer::new().format_finite(value).to_string(),
                }
            }
            Constant::Str(str_id) => match self.printer.strings.get(str_id.value()) {
                Some(text) => format!("{text:?}"),
                None => format!("str#{}", str_id.value()),
            },
        }
    }

    fn terminator(&mut self, terminator: Terminator) {
        let text = match terminator {
            Terminator::Goto(target) => format!("goto block{}", target.index()),
            Terminator::Branch {
                then_block,
                else_block,
                ..
            } => format!(
                "branch block{} block{}",
                then_block.index(),
                else_block.index()
            ),
            Terminator::Switch {
                targets, default, ..
            } => {
                let mut text = "switch".to_string();
                for target in self.arena.items(targets) {
                    let _ = write!(text, " block{}", target.index());
                }
                let _ = write!(text, " default block{}", default.index());
                text
            }
            Terminator::Return(_) => "return".to_string(),
            Terminator::Unreachable => "unreachable".to_string(),
            Terminator::RuntimeError(message) => {
                let constant = self.printer.tables.constants.get(message);
                format!("runtime_error {}", self.constant(constant))
            }
        };
        self.line(1, text);
        if let Some(operand) = terminator.operand() {
            self.tree(operand, 2);
        }
    }
}

impl PrintableShape for Structured {
    const LEVEL_NAME: &'static str = "structured";

    fn print_sections(&self, writer: &mut Writer) {
        writer.line(0, "body".to_string());
        for &statement in writer.arena.items(self.body) {
            writer.tree(statement, 1);
        }
        if let Some(result) = self.result {
            writer.line(0, "result".to_string());
            writer.tree(result, 1);
        }
    }
}

impl PrintableShape for Flat {
    const LEVEL_NAME: &'static str = "flat";

    fn print_sections(&self, writer: &mut Writer) {
        for (block_id, block) in self.blocks.iter() {
            let entry = if block_id == self.entry {
                " (entry)"
            } else {
                ""
            };
            writer.line(0, format!("block{}{entry}", block_id.index()));
            for &statement in writer.arena.items(block.statements) {
                writer.tree(statement, 1);
            }
            writer.terminator(block.terminator);
        }
    }
}

fn type_name(pinp_type: PinpType) -> String {
    match pinp_type {
        PinpType::Bool => "bool".to_string(),
        PinpType::Int => "int".to_string(),
        PinpType::Float => "float".to_string(),
        PinpType::Str => "str".to_string(),
        PinpType::Void => "void".to_string(),
        PinpType::Never => "never".to_string(),
        PinpType::Range => "range".to_string(),
        PinpType::Array(element, length) => format!("{}[{length}]", element_name(element)),
        PinpType::Matrix(element, rows, columns) => {
            format!("{}[{rows}x{columns}]", element_name(element))
        }
    }
}

fn element_name(element: ArrayElementType) -> &'static str {
    match element {
        ArrayElementType::Bool => "bool",
        ArrayElementType::Int => "int",
        ArrayElementType::Float => "float",
    }
}

fn scalar_name(stored: ScalarKind) -> &'static str {
    match stored {
        ScalarKind::Byte => "byte",
        ScalarKind::Word => "word",
    }
}

fn binary_word(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "add",
        BinOp::Sub => "subtract",
        BinOp::Mul => "multiply",
        BinOp::Div => "divide",
        BinOp::IntDiv => "div",
        BinOp::Mod => "mod",
        BinOp::Pow => "power",
        BinOp::Eq => "equal",
        BinOp::Ne => "not_equal",
        BinOp::Lt => "less",
        BinOp::Gt => "greater",
        BinOp::Le => "less_equal",
        BinOp::Ge => "greater_equal",
        BinOp::And => "and",
        BinOp::Or => "or",
        BinOp::Xor => "xor",
    }
}
