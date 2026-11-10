// SPDX-License-Identifier: MIT

//! Liveness: which variables still have a read ahead of them.
//!
//! A backward analysis: its state is the set of live variables, and a read makes a variable live
//! while a write kills it. Terms shared across this layer — lattice, join, direction, diagnostic —
//! are defined in [`crate::analysis`].
//!
//! Vocabulary particular to liveness:
//!
//! * Variable: a local of the function — or, at the top level, a global no function touches,
//!   which only the top level can read or write. Each binding is its own variable, even when two
//!   share a name.
//! * Read, write: a use of a variable's value, and a store into it.
//! * Live: a variable some later read reaches on at least one path. Joining unions the sets,
//!   because a variable read on *any* onward path is still needed.
//! * Kill: what a write does to the value that was there.
//! * Dead store: a write to a variable that is not live immediately afterwards — the value is
//!   replaced or discarded before anything looks at it.
//! * Unused variable: a variable with no read anywhere.
//!
//! The last two are this module's diagnostics, kept as separate rules. The remedies differ: a dead
//! store is one assignment to delete; an unused variable is the binding and every write to it.
//! A variable nothing reads yields an unused-variable diagnostic at its introduction and a dead store
//! at each later write, matching what `rustc` reports for the same shape. Compiler temporaries have
//! no name and are never reported; nor is a variable named `_`.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::analysis::dataflow::{
    Analysis, Direction, Lattice, VariableSet, replay_backward, solve,
};
use crate::diagnostic::{Diagnostic, DiagnosticCode};
use crate::ir::{
    Flat, Function, GlobalId, LocalId, NodeId, Operation, Program, ProgramTables, RuntimeFunction,
    Terminator,
};
use crate::lexer::Span;
use crate::parser::SymId;

/// Reports dead stores and unused variables in `function`. `entry_private_globals` are the globals
/// analysed like its locals: at the top level, those no other function reads or writes.
pub fn check_liveness(
    function: &Function<Flat>,
    tables: &ProgramTables,
    names: &[&str],
    entry_private_globals: &FxHashSet<GlobalId>,
) -> Vec<Diagnostic> {
    let analysis = LiveVariables {
        function,
        entry_private_globals,
    };
    let solution = solve(function, &analysis);
    let blocks = &function.shape.blocks;

    let mut ever_read = VariableSet::default();
    let mut introduction: FxHashMap<usize, Span> = FxHashMap::default();
    for (_, block) in blocks.iter() {
        for &statement in function.arena.items(block.statements) {
            analysis.reads_in(statement, &mut ever_read);
            if let Some(variable) = analysis.written(statement) {
                let span = function.arena.spans[statement];
                introduction
                    .entry(variable)
                    .and_modify(|first| {
                        if span.start < first.start {
                            *first = span;
                        }
                    })
                    .or_insert(span);
            }
        }
        if let Some(operand) = block.terminator.operand() {
            analysis.reads_in(operand, &mut ever_read);
        }
    }

    let mut diagnostics = Vec::new();
    let mut reported_unused = FxHashSet::default();
    for (block_id, _) in blocks.iter() {
        replay_backward(
            function,
            &analysis,
            &solution,
            block_id,
            |statement, live| {
                let Some(variable) = analysis.written(statement) else {
                    return;
                };
                let Some(name) = analysis.name(variable, tables, names) else {
                    return;
                };
                if name == "_" {
                    return;
                }
                let span = function.arena.spans[statement];
                let introduces = introduction.get(&variable) == Some(&span);
                if !ever_read.contains(variable) && introduces && reported_unused.insert(variable) {
                    diagnostics.push(Diagnostic::new(
                        DiagnosticCode::UnusedVariable,
                        span,
                        format!("Variable `{name}` is never used."),
                    ));
                } else if !live.contains(variable) {
                    diagnostics.push(Diagnostic::new(
                        DiagnosticCode::DeadStore,
                        span,
                        format!("Value assigned to `{name}` is never read."),
                    ));
                }
            },
        );
    }
    diagnostics
}

/// The globals private to the top level: every global no other function reads or writes. Those are
/// analysed like the top level's locals; any other is live wherever the top level ends.
pub fn entry_private_globals(program: &Program<Flat>) -> FxHashSet<GlobalId> {
    let mut private: FxHashSet<GlobalId> = program
        .tables
        .globals
        .iter()
        .map(|(global_id, _)| global_id)
        .collect();
    for (_, function) in program.functions.iter() {
        if function.id == program.entry {
            continue;
        }
        for (_, operation) in function.arena.operations.iter() {
            if let Operation::ReadGlobal(global_id) | Operation::WriteGlobal { global_id, .. } =
                operation
            {
                private.remove(global_id);
            }
        }
    }
    private
}

struct LiveVariables<'function> {
    function: &'function Function<Flat>,
    entry_private_globals: &'function FxHashSet<GlobalId>,
}

impl Analysis for LiveVariables<'_> {
    type Lattice = VariableSet;
    const DIRECTION: Direction = Direction::Backward;

    fn boundary(&self) -> VariableSet {
        VariableSet::bottom()
    }

    /// One growth per variable that can join the set.
    fn height(&self, function: &Function<Flat>) -> usize {
        function.arena.locals.len() + self.entry_private_globals.len()
    }

    fn transfer_statement(&self, statement: NodeId, state: &mut VariableSet) {
        if let Some(variable) = self.written(statement) {
            state.remove(variable);
        }
        self.reads_in(statement, state);
    }

    fn transfer_terminator(&self, terminator: &Terminator, state: &mut VariableSet) {
        if let Some(operand) = terminator.operand() {
            self.reads_in(operand, state);
        }
    }
}

impl LiveVariables<'_> {
    /// Locals are numbered first, the entry's private globals after them.
    fn local(&self, local_id: LocalId) -> usize {
        local_id.index() as usize
    }

    fn global(&self, global_id: GlobalId) -> Option<usize> {
        self.entry_private_globals
            .contains(&global_id)
            .then(|| self.function.arena.locals.len() + global_id.index() as usize)
    }

    /// The variable a statement stores into as a whole, if any.
    fn written(&self, statement: NodeId) -> Option<usize> {
        match self.function.arena.operations[statement] {
            Operation::WriteLocal { local_id, .. } => Some(self.local(local_id)),
            Operation::WriteGlobal { global_id, .. } => self.global(global_id),
            _ => None,
        }
    }

    /// Adds every variable read in the tree under `root`. Freeing a string is not reading it: the
    /// frees ownership places before every string store and at the end would otherwise make every
    /// string look used.
    fn reads_in(&self, root: NodeId, read: &mut VariableSet) {
        let arena = &self.function.arena;
        let mut pending = vec![root];
        while let Some(node_id) = pending.pop() {
            match arena.operations[node_id] {
                Operation::RuntimeCall {
                    function: RuntimeFunction::StrFree,
                    ..
                } => continue,
                Operation::ReadLocal(local_id) => {
                    read.insert(self.local(local_id));
                }
                Operation::ReadGlobal(global_id) => {
                    if let Some(variable) = self.global(global_id) {
                        read.insert(variable);
                    }
                }
                _ => {}
            }
            arena.for_each_child(node_id, |child| pending.push(child));
        }
    }

    /// The source name of a variable; `None` for a compiler temporary.
    fn name<'names>(
        &self,
        variable: usize,
        tables: &ProgramTables,
        names: &[&'names str],
    ) -> Option<&'names str> {
        let locals = &self.function.arena.locals;
        let sym_id: SymId = match variable.checked_sub(locals.len()) {
            None => locals[LocalId::from_index(variable as u32)].name?,
            Some(global) => tables.globals[GlobalId::from_index(global as u32)].name,
        };
        Some(names[sym_id.value()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desugar::desugar;
    use crate::flatten::flatten_program;
    use crate::parser::parse;
    use crate::sema::analyze;
    use indoc::indoc;

    /// Diagnostics for the function at `index` of `src` (the entry is the last), as `(code, text)`
    /// pairs sorted — with the globals analysed the way `analysis::check` analyses them.
    fn diagnostics_in(src: &str, index: usize) -> Vec<(&'static str, String)> {
        let mut ast = parse(src).unwrap();
        analyze(&mut ast).unwrap();
        let program = flatten_program(desugar(&ast));
        let private = entry_private_globals(&program);
        let function = program.functions.iter().nth(index).unwrap().1;
        let private = match function.id == program.entry {
            true => private,
            false => FxHashSet::default(),
        };
        let mut found: Vec<(&'static str, String)> =
            check_liveness(function, &program.tables, &ast.names, &private)
                .into_iter()
                .map(|diagnostic| {
                    let code = diagnostic
                        .code
                        .expect("a checker's diagnostic carries a code");
                    (code.name(), diagnostic.span.text(src).to_string())
                })
                .collect();
        found.sort();
        found
    }

    /// Diagnostics for the top-level program.
    fn diagnostics(src: &str) -> Vec<(&'static str, String)> {
        let mut ast = parse(src).unwrap();
        analyze(&mut ast).unwrap();
        let entry = flatten_program(desugar(&ast)).entry;
        diagnostics_in(src, entry.index() as usize)
    }

    fn found(pairs: &[(&'static str, &str)]) -> Vec<(&'static str, String)> {
        pairs
            .iter()
            .map(|&(code, text)| (code, text.to_string()))
            .collect()
    }

    #[test]
    fn a_value_read_later_is_not_reported() {
        assert!(diagnostics("a = 1\na + 1").is_empty());
    }

    #[test]
    fn an_overwritten_value_is_a_dead_store() {
        assert_eq!(
            diagnostics("a = 1\na = 2\na"),
            found(&[("dead-store", "a")])
        );
    }

    #[test]
    fn a_variable_nothing_reads_is_an_unused_variable() {
        assert_eq!(diagnostics("a = 1\n0"), found(&[("unused-variable", "a")]));
    }

    #[test]
    fn a_never_read_variable_reports_its_introduction_then_dead_stores() {
        assert_eq!(
            diagnostics("a = 1\na = 2\na = 3\n0"),
            found(&[
                ("dead-store", "a"),
                ("dead-store", "a"),
                ("unused-variable", "a")
            ])
        );
    }

    #[test]
    fn an_unused_variable_is_reported_at_its_introduction() {
        let src = "alpha = 1\nalpha = 2\nalpha = 3\n0";
        let mut ast = parse(src).unwrap();
        analyze(&mut ast).unwrap();
        let program = flatten_program(desugar(&ast));
        let entry = &program.functions[program.entry];
        let unused: Vec<u32> = check_liveness(
            entry,
            &program.tables,
            &ast.names,
            &entry_private_globals(&program),
        )
        .into_iter()
        .filter(|diagnostic| diagnostic.code == Some(DiagnosticCode::UnusedVariable))
        .map(|diagnostic| diagnostic.span.start)
        .collect();
        assert_eq!(unused, vec![0], "the first `alpha`, at offset 0");
    }

    #[test]
    fn a_self_referring_update_keeps_the_binding_live() {
        assert!(diagnostics("a = 1\na = a + 1\na").is_empty());
    }

    #[test]
    fn a_value_read_only_inside_a_loop_is_live() {
        assert!(diagnostics("a = 1\nn = 0\nwhile n < 3\n    n += a\nn").is_empty());
    }

    #[test]
    fn a_store_in_a_loop_read_next_iteration_is_live() {
        assert!(diagnostics("total = 0\nfor idx in 1..3\n    total += idx\ntotal").is_empty());
    }

    #[test]
    fn a_value_read_on_only_one_branch_is_live() {
        assert!(diagnostics("a = 1\nb = 0\nif b > 0\n    b = a\nb").is_empty());
    }

    #[test]
    fn the_dont_care_binder_is_never_reported() {
        assert!(
            diagnostics(indoc! {"
                m = [1, 2; 3, 4]
                total = 0
                for _, _, val in m
                    total += val
                total
            "})
            .is_empty()
        );
    }

    #[test]
    fn an_unread_range_loop_index_is_not_reported() {
        // Idiomatic "repeat n times"; the loop's own test and step read the index.
        assert!(diagnostics("total = 0\nfor idx in 1..3\n    total += 1\ntotal").is_empty());
    }

    #[test]
    fn an_unread_array_loop_value_is_an_unused_variable() {
        // Newly reported, as `rustc` does; `_` silences it.
        assert_eq!(
            diagnostics("a = [1, 2]\nt = 0\nfor v in a\n    t += 1\nt"),
            found(&[("unused-variable", "v")])
        );
        assert!(diagnostics("a = [1, 2]\nt = 0\nfor _ in a\n    t += 1\nt").is_empty());
    }

    #[test]
    fn an_unread_index_binder_is_the_loop_index_and_not_reported() {
        assert!(diagnostics("a = [1, 2]\nt = 0\nfor i, v in a\n    t += v\nt").is_empty());
    }

    #[test]
    fn an_unread_float_comprehension_variable_is_an_unused_variable() {
        assert_eq!(
            diagnostics("[1.0 for x: float in 0..3]"),
            found(&[("unused-variable", "x")])
        );
        assert!(
            diagnostics("[1 for x in 0..3]").is_empty(),
            "an int one is the loop index"
        );
    }

    #[test]
    fn a_global_read_from_a_function_is_not_reported() {
        assert!(diagnostics("g = 1\nread(): int is ::g\nread()").is_empty());
    }

    #[test]
    fn a_global_written_in_a_function_is_not_a_dead_store() {
        let src = indoc! {"
            g = 1
            bump(): int is
                ::g = ::g + 1
                0
            bump()
        "};
        assert!(diagnostics_in(src, 0).is_empty(), "in the function");
        assert!(diagnostics(src).is_empty(), "at the top level");
    }

    #[test]
    fn a_global_no_function_touches_is_analysed_like_a_local() {
        let src = indoc! {"
            g = 1
            other = 1
            f(): int is ::g
            other = 2
            f() + other
        "};
        assert_eq!(diagnostics(src), found(&[("dead-store", "other")]));
    }

    #[test]
    fn a_function_local_nothing_reads_is_reported() {
        let src = "f(n: int): int is\n    unused = n * 2\n    n\nf(1)";
        assert_eq!(
            diagnostics_in(src, 0),
            found(&[("unused-variable", "unused")])
        );
    }

    #[test]
    fn an_unread_parameter_is_not_reported() {
        assert!(diagnostics_in("f(n: int): int is 5\nf(1)", 0).is_empty());
    }

    #[test]
    fn a_function_result_counts_as_a_read() {
        let src = "f(n: int): int is\n    doubled = n * 2\n    doubled\nf(1)";
        assert!(diagnostics_in(src, 0).is_empty());
    }

    #[test]
    fn a_read_inside_an_if_expression_counts() {
        assert!(diagnostics("a = 1\nb = a if a > 0 else 0\nb").is_empty());
    }

    #[test]
    fn a_read_through_interpolation_counts() {
        assert!(diagnostics("name = 'x'\nf'hello {name}'").is_empty());
    }

    #[test]
    fn a_read_through_indexing_counts() {
        assert!(diagnostics("idx = 1\narr = [1, 2, 3]\narr[idx]").is_empty());
    }

    #[test]
    fn writing_one_element_is_not_a_whole_new_value() {
        assert!(diagnostics("arr = [1, 2, 3]\narr[0] = 9\narr[0]").is_empty());
    }

    #[test]
    fn same_named_bindings_in_separate_bodies_are_separate_variables() {
        // Newly precise: by name, the `else` arm's `x` was a dead store of the one variable `x`;
        // as a binding of its own, nothing ever reads it.
        let src = indoc! {"
            c = true
            if c
                x = 1
                x
            else
                x = 2
            0
        "};
        assert_eq!(diagnostics(src), found(&[("unused-variable", "x")]));
    }

    #[test]
    fn swapping_through_a_multiple_assignment_reports_nothing() {
        assert!(diagnostics("a, b = 1, 2\na, b = b, a\na + b").is_empty());
    }

    #[test]
    fn a_chained_assignment_reports_the_unread_target() {
        assert_eq!(
            diagnostics("a = b = 1\na"),
            found(&[("unused-variable", "b")])
        );
    }

    #[test]
    fn compiler_temporaries_are_never_reported() {
        // `and`, `or`, and if-expressions all write temporaries; none has a name to report.
        assert!(
            diagnostics("a = 1\nb = (a > 0 and a < 5) or a == 9\nc = 1 if b else 2\nc").is_empty()
        );
    }

    #[test]
    fn more_variables_than_one_word_of_bits_are_tracked() {
        let mut src = String::from("f(): int is\n");
        for index in 0..70 {
            src += &format!("    v{index} = {index}\n");
        }
        src += "    s = 0\n";
        for index in 0..69 {
            src += &format!("    s += v{index}\n");
        }
        src += "    s\nf()";
        assert_eq!(
            diagnostics_in(&src, 0),
            found(&[("unused-variable", "v69")])
        );
    }

    #[test]
    fn both_diagnostics_can_appear_in_one_run() {
        let src = "dead = 1\ndead = 2\nunread = 3\ndead";
        assert_eq!(
            diagnostics(src),
            found(&[("dead-store", "dead"), ("unused-variable", "unread")])
        );
    }

    #[test]
    fn freeing_a_string_is_not_reading_it() {
        // Every string store frees the old value first, and the end of the program frees them all;
        // neither is a use of the value.
        assert_eq!(
            diagnostics("s = 'a'\ns = 'b'\ns"),
            found(&[("dead-store", "s")])
        );
        assert_eq!(
            diagnostics("s = 'a'\n0"),
            found(&[("unused-variable", "s")])
        );
        let src = "f(): int is\n    t = 'x'\n    0\nf()";
        assert_eq!(diagnostics_in(src, 0), found(&[("unused-variable", "t")]));
    }
}
