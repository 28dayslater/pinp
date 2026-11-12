// SPDX-License-Identifier: MIT

//! Desugaring: the typed AST rewritten as structured-level IR ([`Program`]).
//!
//! *Desugaring* removes the conveniences of the surface language — "syntactic sugar" — by spelling
//! each one out in fewer, plainer operations: `and` becomes an `if`, `x += 1` an ordinary
//! assignment, an implicit promotion an explicit `Convert`. What sema checked, desugaring makes
//! explicit: names become [`LocalId`]s and [`GlobalId`]s, and every node carries its type and the
//! span of the source it came from.
//!
//! One rule shapes most of the code: an expression may need statements of its own (an `if` used as
//! a value, `and`, `or`), which run *before* the statement containing it. Every expression is
//! therefore desugared together with the list of statements it appends to, and an operand to the
//! left of such statements is first saved in a temporary so it is still evaluated first.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::ir::{
    Arena, Constant, Function, FunctionId, Global, GlobalId, IdVec, Local, LocalId, NodeId,
    Operation, Program, ProgramTables, Signature, Structured, TypeId,
};
use crate::lexer::Span;
use crate::parser::{PinpType, Place, ProgramAst, Stmt, SymId, TopLevel};

mod array;
mod expr;
mod ownership;
mod stmt;

#[cfg(test)]
mod tests;

/// Rewrites a type-checked program as structured-level IR, one function per definition plus the
/// entry.
pub fn desugar(ast: &ProgramAst) -> Program {
    let mut desugarer = Desugarer::new(ast);
    for item in &ast.top_level {
        if let TopLevel::Func(func) = item {
            let function_id = desugarer.declare_function(func);
            desugarer.function_ids.insert(func.name, function_id);
        }
    }
    let entry_id = desugarer.declare_entry();
    desugarer.declare_globals();

    let mut functions = Vec::new();
    let mut entry_body = Vec::new();
    let mut entry_result = None;
    let last = ast.top_level.len().wrapping_sub(1);
    for (index, item) in ast.top_level.iter().enumerate() {
        match item {
            TopLevel::Func(func) => {
                let entry_state = std::mem::take(&mut desugarer.function);
                functions.push(desugarer.function_definition(func));
                desugarer.function = entry_state;
            }
            TopLevel::Stmt(stmt) if index == last => {
                entry_result = desugarer.result_statement(stmt, &mut entry_body);
            }
            TopLevel::Stmt(stmt) => desugarer.statement(stmt, &mut entry_body),
        }
    }
    let entry = desugarer.finish(entry_id, 0, &entry_body, entry_result, false);
    let mut program_functions = IdVec::default();
    for function in functions.into_iter().chain([entry]) {
        program_functions.push(function);
    }
    Program {
        tables: desugarer.tables,
        functions: program_functions,
        entry: entry_id,
    }
}

/// What a name resolves to.
#[derive(Debug, Clone, Copy)]
enum Binding {
    Local(LocalId),
    Global(GlobalId),
}

/// The function being built: its arena and the scope frames of its names.
#[derive(Default)]
struct FunctionState {
    arena: Arena,
    /// Innermost last. The first frame is the function's base: its parameters, or — for the entry —
    /// the globals assigned so far.
    scopes: Vec<FxHashMap<SymId, Binding>>,
    is_entry: bool,
    /// The temporaries that move an owned string from where it is made to where it is used; see
    /// [`ownership`].
    ownership_temporaries: FxHashSet<LocalId>,
    /// The declared return type; `None` for the entry, where `return` cannot appear.
    return_type: Option<PinpType>,
    /// Whether a `return` was desugared, so [`Desugarer::finish`] has frees to place before it.
    has_returns: bool,
    /// The user's loops around the statement being desugared, innermost last.
    loops: Vec<UserLoop>,
}

/// A loop as the user wrote it: what `break @label` names, and how many structured loops it became.
struct UserLoop {
    label: Option<SymId>,
    ir_loops: u32,
}

struct Desugarer<'ast> {
    ast: &'ast ProgramAst<'ast>,
    tables: ProgramTables,
    globals: FxHashMap<SymId, GlobalId>,
    function_ids: FxHashMap<SymId, FunctionId>,
    function: FunctionState,
}

impl<'ast> Desugarer<'ast> {
    fn new(ast: &'ast ProgramAst<'ast>) -> Self {
        Desugarer {
            ast,
            tables: ProgramTables::default(),
            globals: FxHashMap::default(),
            function_ids: FxHashMap::default(),
            function: FunctionState {
                scopes: vec![FxHashMap::default()],
                is_entry: true,
                ..FunctionState::default()
            },
        }
    }

    fn declare_function(&mut self, func: &crate::parser::FuncDef) -> FunctionId {
        let parameters = func
            .params
            .iter()
            .map(|param| self.type_id(param.param_type))
            .collect();
        let result = self.type_id(func.return_type);
        self.tables.signatures.push(Signature {
            name: Some(func.name),
            parameters,
            result,
        })
    }

    /// The entry's result is the value of the last top-level statement, when that is an expression
    /// with a value or an assignment.
    fn declare_entry(&mut self) -> FunctionId {
        let result_type = match self.ast.top_level.last() {
            Some(TopLevel::Stmt(Stmt::Expr(expr_id))) => self.ast.type_of(*expr_id),
            Some(TopLevel::Stmt(Stmt::Assign { values, .. })) => self
                .ast
                .type_of(*values.last().expect("an assignment has a value")),
            _ => PinpType::Void,
        };
        let result = self.type_id(result_type);
        self.tables.signatures.push(Signature {
            name: None,
            parameters: Vec::new(),
            result,
        })
    }

    /// Every name assigned by a top-level statement is a global, typed by its first assignment.
    fn declare_globals(&mut self) {
        for item in &self.ast.top_level {
            let TopLevel::Stmt(Stmt::Assign {
                target_lists,
                values,
                ..
            }) = item
            else {
                continue;
            };
            for group in target_lists {
                for (place, value) in group.iter().zip(values) {
                    let (Place::Local(name) | Place::Global(name)) = *place;
                    if self.globals.contains_key(&name) {
                        continue;
                    }
                    let type_id = self.type_id(self.ast.type_of(*value));
                    let global_id = self.tables.globals.push(Global { name, type_id });
                    self.globals.insert(name, global_id);
                }
            }
        }
    }

    fn function_definition(&mut self, func: &crate::parser::FuncDef) -> Function<Structured> {
        let mut parameters = FxHashMap::default();
        for param in &func.params {
            let local_id = self.local(Some(param.name), param.param_type);
            parameters.insert(param.name, Binding::Local(local_id));
        }
        self.function.scopes.push(parameters);
        self.function.return_type = Some(func.return_type);
        let diverges = func.body.stmts.iter().any(|stmt| self.diverges(stmt))
            || func
                .body
                .result
                .is_some_and(|result| self.ast.type_of(result) == PinpType::Never);
        let mut body = Vec::new();
        for stmt in &func.body.stmts {
            self.statement(stmt, &mut body);
        }
        // A body that always returns has no value at its end; a trailing expression the user wrote
        // is still placed, as a statement, so that it can be reported as unreachable.
        let result = match func.body.result {
            Some(result_expr) if !diverges && func.return_type != PinpType::Void => {
                let value = self.expr(result_expr, &mut body);
                Some(self.convert(value, func.return_type))
            }
            Some(result_expr) => {
                self.effect(result_expr, &mut body);
                None
            }
            None => None,
        };
        let function_id = self.function_ids[&func.name];
        self.finish(
            function_id,
            func.params.len() as u32,
            &body,
            result,
            diverges,
        )
    }

    /// Whether control never continues past `stmt`, as sema typed it.
    fn diverges(&self, stmt: &Stmt) -> bool {
        match stmt {
            Stmt::Return { .. } => true,
            Stmt::Expr(expr_id) => self.ast.type_of(*expr_id) == PinpType::Never,
            _ => false,
        }
    }

    /// Completes a function: the frees before every `return` and, unless the body `diverges`, at
    /// its end. Code placed at the end of a diverging body could never run, and would be reported
    /// as unreachable.
    fn finish(
        &mut self,
        id: FunctionId,
        parameter_count: u32,
        body: &[NodeId],
        result: Option<NodeId>,
        diverges: bool,
    ) -> Function<Structured> {
        let mut body = match self.function.has_returns {
            true => self.place_return_frees(body),
            false => body.to_vec(),
        };
        let result = match diverges {
            true => None,
            false => {
                let end_span = match (result, body.last()) {
                    (Some(node), _) | (None, Some(&node)) => self.function.arena.spans[node],
                    (None, None) => Span::UNKNOWN,
                };
                self.free_on_exit(result, end_span, &mut body)
            }
        };
        let mut arena = std::mem::take(&mut self.function).arena;
        let body = arena.list(&body);
        let function = Function {
            id,
            parameter_count,
            arena,
            shape: Structured { body, result },
        };
        debug_assert_eq!(
            crate::ir::verify(&function),
            Ok(()),
            "desugaring broke the IR"
        );
        function
    }

    // -------------------------------------------------------------------------
    // Building blocks
    // -------------------------------------------------------------------------

    fn type_id(&mut self, pinp_type: PinpType) -> TypeId {
        self.tables.types.intern(pinp_type)
    }

    fn push(&mut self, operation: Operation, pinp_type: PinpType, span: Span) -> NodeId {
        let type_id = self.type_id(pinp_type);
        self.function.arena.push(operation, type_id, span)
    }

    fn statement_node(&mut self, operation: Operation, span: Span) -> NodeId {
        self.push(operation, PinpType::Void, span)
    }

    fn node_type(&self, node_id: NodeId) -> PinpType {
        let type_id = self.function.arena.types[node_id].expect("desugared nodes are typed");
        self.tables.types.get(type_id)
    }

    fn local(&mut self, name: Option<SymId>, pinp_type: PinpType) -> LocalId {
        let type_id = self.type_id(pinp_type);
        self.function.arena.locals.push(Local { name, type_id })
    }

    /// A compiler temporary: a local with no name, which checkers never report.
    fn temporary(&mut self, pinp_type: PinpType) -> LocalId {
        self.local(None, pinp_type)
    }

    fn read_local(&mut self, local_id: LocalId, span: Span) -> NodeId {
        let type_id = self.function.arena.locals[local_id].type_id;
        let pinp_type = self.tables.types.get(type_id);
        self.push(Operation::ReadLocal(local_id), pinp_type, span)
    }

    fn write_local(&mut self, local_id: LocalId, value: NodeId, span: Span) -> NodeId {
        self.statement_node(Operation::WriteLocal { local_id, value }, span)
    }

    /// `value` as `to`: the node itself when it already has that type, otherwise a `Convert`
    /// spanned by the value it converts.
    fn convert(&mut self, value: NodeId, to: PinpType) -> NodeId {
        if self.node_type(value) == to {
            return value;
        }
        let span = self.function.arena.spans[value];
        self.push(Operation::Convert { operand: value }, to, span)
    }

    fn binding_type(&self, binding: Binding) -> PinpType {
        let type_id = match binding {
            Binding::Local(local_id) => self.function.arena.locals[local_id].type_id,
            Binding::Global(global_id) => self.tables.globals[global_id].type_id,
        };
        self.tables.types.get(type_id)
    }

    /// The nearest enclosing binding of `name`.
    fn lookup(&self, name: SymId) -> Option<Binding> {
        self.function
            .scopes
            .iter()
            .rev()
            .find_map(|frame| frame.get(&name).copied())
    }

    /// Runs `inside` in a new innermost scope frame, whose names do not escape it.
    fn scoped<T>(&mut self, inside: impl FnOnce(&mut Self) -> T) -> T {
        self.function.scopes.push(FxHashMap::default());
        let result = inside(self);
        self.function.scopes.pop();
        result
    }

    /// The binding an assignment to `place` writes, creating it on first assignment: a global at
    /// the top level of the entry, otherwise a local of the innermost scope.
    fn assignment_target(&mut self, place: Place, value_type: PinpType) -> Binding {
        match place {
            Place::Global(name) => Binding::Global(self.globals[&name]),
            Place::Local(name) => {
                if let Some(binding) = self.lookup(name) {
                    return binding;
                }
                let binding = if self.function.is_entry && self.function.scopes.len() == 1 {
                    Binding::Global(self.globals[&name])
                } else {
                    Binding::Local(self.local(Some(name), value_type))
                };
                let frame = self.function.scopes.last_mut().expect("a scope frame");
                frame.insert(name, binding);
                binding
            }
        }
    }

    fn constant(&mut self, constant: Constant, pinp_type: PinpType, span: Span) -> NodeId {
        let constant_id = self.tables.constants.intern(constant);
        self.push(Operation::Constant(constant_id), pinp_type, span)
    }
}
