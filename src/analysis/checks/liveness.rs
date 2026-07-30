// SPDX-License-Identifier: MIT

//! Liveness: which bindings still have a read ahead of them.
//!
//! A backward analysis — the fact at a point is the set of symbols something later will read. A
//! read makes a symbol live, a write kills it (whatever was there is gone), and at a merge the sets
//! union, because a symbol read on *any* onward path is still needed.
//!
//! Two findings fall out of the solution:
//!
//! * a write to a symbol that is not live immediately afterwards is a **dead store** — the value is
//!   replaced or discarded before anything looks at it;
//! * a symbol with no read anywhere is an **unused binding**.
//!
//! They are separate rules because the fix differs, and because reporting every store to a
//! never-read binding would bury the one line worth changing.

use rustc_hash::FxHashSet;

use crate::analysis::cfg::{Action, Cfg, Terminator};
use crate::analysis::dataflow::{Analysis, Direction, MergeableFact, SymbolSetFact, solve};
use crate::analysis::diagnostic::{Diagnostic, DiagnosticCode};
use crate::lexer::Span;
use crate::parser::{Block, ExprId, FStrSegment, Node, Place, ProgramAst, Stmt, SymId};

/// Reports dead stores and unused bindings in one graph.
///
/// `escaping` are symbols that outlive this graph — globals, which another function may read — and
/// are treated as live wherever the graph ends. Compute them once with [`escaping_symbols`].
pub fn check(ast: &ProgramAst, cfg: &Cfg, escaping: &FxHashSet<SymId>) -> Vec<Diagnostic> {
    let analysis = Liveness { ast, escaping };
    let solution = solve(cfg, &analysis);

    // Every symbol anything reads, anywhere in this graph: what tells an unused binding from a
    // merely dead store.
    let mut ever_read: FxHashSet<SymId> = FxHashSet::default();
    for block_id in cfg.block_ids() {
        for action in &cfg.block(block_id).actions {
            analysis.reads_of_action(action, &mut ever_read);
        }
        analysis.reads_of_terminator(&cfg.block(block_id).terminator, &mut ever_read);
    }

    let mut diagnostics = Vec::new();
    // First write per symbol, so an unused binding is reported once, where it is introduced.
    let mut reported_unused: FxHashSet<SymId> = FxHashSet::default();

    for block_id in cfg.block_ids() {
        // Re-walk the block backwards from its outgoing fact to get the fact at each action; the
        // solver stores facts per block, and per-action facts are cheaper to recompute than to keep.
        let mut live = solution.after(block_id).clone();
        analysis.transfer_terminator(&cfg.block(block_id).terminator, &mut live);

        for action in cfg.block(block_id).actions.iter().rev() {
            // `live` is what is needed *after* this action — exactly the question a dead store asks.
            let Action::Statement(stmt) = action else {
                analysis.transfer_action(action, &mut live);
                continue;
            };
            for (sym_id, span) in analysis.writes_of_stmt(stmt) {
                if analysis.is_exempt(sym_id) {
                    continue;
                }
                if !ever_read.contains(&sym_id) {
                    if reported_unused.insert(sym_id) {
                        diagnostics.push(Diagnostic::new(
                            DiagnosticCode::UnusedBinding,
                            span,
                            format!("Binding `{}` is never read.", analysis.name(sym_id)),
                        ));
                    }
                } else if !live.contains(sym_id) {
                    diagnostics.push(Diagnostic::new(
                        DiagnosticCode::DeadStore,
                        span,
                        format!(
                            "Value assigned to `{}` is never read.",
                            analysis.name(sym_id)
                        ),
                    ));
                }
            }
            analysis.transfer_action(action, &mut live);
        }
    }
    diagnostics
}

/// Symbols whose life does not end with any one graph: everything named through `::`.
///
/// A function writing `::total` is not making a dead store even if it never reads it back, and a
/// global the entry assigns may be read only from inside a function. Both directions are covered by
/// treating every `::`-named symbol as live wherever a graph ends.
///
/// Deliberately coarse: it counts a `::name` on the left of an assignment as well as a read, so a
/// global nothing ever reads is not reported. Over-approximating liveness only ever *suppresses* a
/// finding, which is the safe direction.
pub fn escaping_symbols(ast: &ProgramAst) -> FxHashSet<SymId> {
    let mut escaping = FxHashSet::default();
    for node in &ast.nodes {
        if let Node::Global(sym_id) = node {
            escaping.insert(*sym_id);
        }
    }
    for item in &ast.top_level {
        collect_global_targets(top_level_stmt(item), &mut escaping);
    }
    escaping
}

fn top_level_stmt(item: &crate::parser::TopLevel) -> Option<&Stmt> {
    match item {
        crate::parser::TopLevel::Stmt(stmt) => Some(stmt),
        crate::parser::TopLevel::Func(_) => None,
    }
}

fn collect_global_targets(stmt: Option<&Stmt>, escaping: &mut FxHashSet<SymId>) {
    let Some(Stmt::Assign { target_lists, .. }) = stmt else {
        return;
    };
    for group in target_lists {
        for place in group {
            if let Place::Global(sym_id) = place {
                escaping.insert(*sym_id);
            }
        }
    }
}

struct Liveness<'a, 'ast> {
    ast: &'a ProgramAst<'ast>,
    escaping: &'a FxHashSet<SymId>,
}

impl<'ast> Analysis<'ast> for Liveness<'_, 'ast> {
    type Fact = SymbolSetFact;
    const DIRECTION: Direction = Direction::Backward;

    fn boundary(&self) -> SymbolSetFact {
        // Where the graph ends, everything that outlives it is still live.
        let mut fact = SymbolSetFact::nothing_known();
        for sym_id in self.escaping {
            fact.insert(*sym_id);
        }
        fact
    }

    fn transfer_action(&self, action: &Action<'ast>, fact: &mut SymbolSetFact) {
        match action {
            Action::Statement(stmt) => {
                // Kill first, then revive: a statement's own reads happen before its write lands,
                // so `s = s + 1` leaves `s` live going backwards.
                for (sym_id, _) in self.writes_of_stmt(stmt) {
                    fact.remove(sym_id);
                }
                let mut reads = FxHashSet::default();
                self.reads_of_stmt(stmt, &mut reads);
                for sym_id in reads {
                    fact.insert(sym_id);
                }
            }
            Action::Evaluate(expr_id) => {
                let mut reads = FxHashSet::default();
                self.reads_of_expr(*expr_id, &mut reads);
                for sym_id in reads {
                    fact.insert(sym_id);
                }
            }
            // A loop variable is written afresh each iteration, so anything live before the binding
            // is not this binding's value.
            Action::Bind(sym_id) => {
                fact.remove(*sym_id);
            }
        }
    }

    fn transfer_terminator(&self, terminator: &Terminator, fact: &mut SymbolSetFact) {
        let mut reads = FxHashSet::default();
        self.reads_of_terminator(terminator, &mut reads);
        for sym_id in reads {
            fact.insert(sym_id);
        }
    }
}

impl Liveness<'_, '_> {
    fn name(&self, sym_id: SymId) -> &str {
        self.ast.names[sym_id.value()]
    }

    /// Whether a symbol is deliberately not reported about.
    fn is_exempt(&self, sym_id: SymId) -> bool {
        // `_` exists to say "I am not using this".
        self.name(sym_id) == "_"
            // A global may be read from a function this graph knows nothing about.
            || self.escaping.contains(&sym_id)
    }

    fn reads_of_action(&self, action: &Action<'_>, out: &mut FxHashSet<SymId>) {
        match action {
            Action::Statement(stmt) => self.reads_of_stmt(stmt, out),
            Action::Evaluate(expr_id) => self.reads_of_expr(*expr_id, out),
            Action::Bind(_) => {}
        }
    }

    fn reads_of_terminator(&self, terminator: &Terminator, out: &mut FxHashSet<SymId>) {
        match terminator {
            Terminator::Branch { cond, .. } => self.reads_of_expr(*cond, out),
            Terminator::Return(Some(result)) => self.reads_of_expr(*result, out),
            Terminator::Goto(_) | Terminator::Return(None) | Terminator::Exit => {}
        }
    }

    /// The bindings a statement writes, with where each was written.
    fn writes_of_stmt(&self, stmt: &Stmt) -> Vec<(SymId, Span)> {
        match stmt {
            Stmt::Assign {
                target_lists,
                target_spans,
                ..
            } => target_lists
                .iter()
                .zip(target_spans)
                .flat_map(|(places, spans)| places.iter().zip(spans))
                .map(|(place, span)| (place_sym(*place), *span))
                .collect(),
            // An indexed assignment updates one element of an existing array; the binding itself is
            // read, not replaced, so it is not a write for liveness purposes.
            _ => Vec::new(),
        }
    }

    /// Every binding a statement reads, including inside nested blocks.
    fn reads_of_stmt(&self, stmt: &Stmt, out: &mut FxHashSet<SymId>) {
        match stmt {
            Stmt::Assign { values, .. } => {
                for value in values {
                    self.reads_of_expr(*value, out);
                }
            }
            Stmt::Expr(expr_id) => self.reads_of_expr(*expr_id, out),
            Stmt::While { cond, body } => {
                self.reads_of_expr(*cond, out);
                self.reads_of_block(body, out);
            }
            Stmt::Loop { body, cond, .. } => {
                self.reads_of_block(body, out);
                self.reads_of_expr(*cond, out);
            }
            Stmt::For { source, body, .. } | Stmt::ForArray { source, body, .. } => {
                self.reads_of_expr(*source, out);
                self.reads_of_block(body, out);
            }
            Stmt::IndexedAssign {
                target,
                index,
                value,
                ..
            } => {
                out.insert(place_sym(*target));
                self.reads_of_expr(*index, out);
                self.reads_of_expr(*value, out);
            }
            Stmt::IndexedAssign2D {
                target,
                row,
                col,
                value,
                ..
            } => {
                out.insert(place_sym(*target));
                self.reads_of_expr(*row, out);
                self.reads_of_expr(*col, out);
                self.reads_of_expr(*value, out);
            }
        }
    }

    fn reads_of_block(&self, block: &Block, out: &mut FxHashSet<SymId>) {
        for stmt in &block.stmts {
            self.reads_of_stmt(stmt, out);
        }
        if let Some(result) = block.result {
            self.reads_of_expr(result, out);
        }
    }

    /// Every binding an expression reads.
    ///
    /// Nested blocks — an `if`-expression's arms, which the CFG treats as part of one atomic
    /// statement — are walked for their reads. Writes inside them are *not* collected, which loses
    /// a finding rather than inventing one.
    fn reads_of_expr(&self, expr_id: ExprId, out: &mut FxHashSet<SymId>) {
        match self.ast.node(expr_id) {
            Node::Var(sym_id) | Node::Global(sym_id) => {
                out.insert(*sym_id);
            }
            Node::FStr { segments } => {
                for segment in segments {
                    if let FStrSegment::Interp(place) = segment {
                        out.insert(place_sym(*place));
                    }
                }
            }
            Node::Unary { operand, .. } => self.reads_of_expr(*operand, out),
            Node::Bin { lhs, rhs, .. } => {
                self.reads_of_expr(*lhs, out);
                self.reads_of_expr(*rhs, out);
            }
            Node::Call { args, .. } => {
                for arg in args {
                    self.reads_of_expr(*arg, out);
                }
            }
            Node::If { arms, else_block } => {
                for arm in arms {
                    self.reads_of_expr(arm.cond, out);
                    self.reads_of_block(&arm.body, out);
                }
                if let Some(else_body) = else_block {
                    self.reads_of_block(else_body, out);
                }
            }
            Node::Range {
                start, stop, step, ..
            } => {
                self.reads_of_expr(*start, out);
                self.reads_of_expr(*stop, out);
                if let Some(step) = step {
                    self.reads_of_expr(*step, out);
                }
            }
            Node::Membership { value, range } => {
                self.reads_of_expr(*value, out);
                self.reads_of_expr(*range, out);
            }
            Node::ArrayLiteral { elements } => {
                for element in elements {
                    self.reads_of_expr(*element, out);
                }
            }
            Node::MatrixLiteral { rows } => {
                for row in rows {
                    for element in row {
                        self.reads_of_expr(*element, out);
                    }
                }
            }
            Node::Index { array, index } => {
                self.reads_of_expr(*array, out);
                self.reads_of_expr(*index, out);
            }
            Node::Index2D { matrix, row, col } => {
                self.reads_of_expr(*matrix, out);
                self.reads_of_expr(*row, out);
                self.reads_of_expr(*col, out);
            }
            Node::Member { object, .. } => self.reads_of_expr(*object, out),
            Node::Comprehension {
                element, source, ..
            } => {
                self.reads_of_expr(*element, out);
                self.reads_of_expr(*source, out);
            }
            Node::Int(_) | Node::Float(_) | Node::Bool(_) | Node::Str(_) | Node::FullExtent => {}
        }
    }
}

fn place_sym(place: Place) -> SymId {
    match place {
        Place::Local(sym_id) | Place::Global(sym_id) => sym_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::diagnostic::DiagnosticCode;
    use crate::parser::{TopLevel, parse};
    use indoc::indoc;

    /// Findings for the top-level program, as `(code, source text)` pairs in source order.
    fn findings(src: &str) -> Vec<(&'static str, String)> {
        let ast = parse(src).unwrap();
        let cfg = Cfg::build_entry(&ast);
        let escaping = escaping_symbols(&ast);
        let mut found: Vec<(&'static str, String)> = check(&ast, &cfg, &escaping)
            .into_iter()
            .map(|diagnostic| {
                (
                    diagnostic.code.name(),
                    diagnostic.span.text(src).to_string(),
                )
            })
            .collect();
        found.sort();
        found
    }

    /// Findings for the first function defined in `src`.
    fn function_findings(src: &str) -> Vec<(&'static str, String)> {
        let ast = parse(src).unwrap();
        let TopLevel::Func(func) = &ast.top_level[0] else {
            panic!("Expected a function first.");
        };
        let cfg = Cfg::build_function(&ast, func);
        let escaping = escaping_symbols(&ast);
        check(&ast, &cfg, &escaping)
            .into_iter()
            .map(|diagnostic| {
                (
                    diagnostic.code.name(),
                    diagnostic.span.text(src).to_string(),
                )
            })
            .collect()
    }

    #[test]
    fn a_value_read_later_is_not_reported() {
        assert!(findings("a = 1\na + 1").is_empty());
    }

    #[test]
    fn an_overwritten_value_is_a_dead_store() {
        assert_eq!(
            findings("a = 1\na = 2\na"),
            vec![("dead-store", "a".to_string())],
            "only the first assignment"
        );
    }

    #[test]
    fn a_binding_nothing_reads_is_unused() {
        assert_eq!(
            findings("a = 1\n0"),
            vec![("unused-binding", "a".to_string())]
        );
    }

    #[test]
    fn an_unused_binding_is_reported_once_however_many_stores() {
        // Three stores, one message: the fix is to the binding, not to each line.
        let found = findings("a = 1\na = 2\na = 3\n0");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, "unused-binding");
    }

    #[test]
    fn a_self_referring_update_keeps_the_binding_live() {
        // `a = a + 1` reads before it writes, so the earlier store is not dead.
        assert!(findings("a = 1\na = a + 1\na").is_empty());
    }

    #[test]
    fn a_value_read_only_inside_a_loop_is_live() {
        // The read is reached through the back edge, which a single pass would miss.
        assert!(
            findings(indoc! {"
                a = 1
                n = 0
                while n < 3
                    n += a
                n
            "})
            .is_empty()
        );
    }

    #[test]
    fn a_store_in_a_loop_read_next_iteration_is_live() {
        assert!(
            findings(indoc! {"
                total = 0
                for idx in 1..3
                    total += idx
                total
            "})
            .is_empty()
        );
    }

    #[test]
    fn a_value_read_on_only_one_branch_is_live() {
        // Liveness unions at a merge: a read on any onward path keeps the value needed.
        assert!(
            findings(indoc! {"
                a = 1
                b = 0
                if b > 0
                    b = a
                b
            "})
            .is_empty()
        );
    }

    #[test]
    fn the_dont_care_binder_is_never_reported() {
        // `_` exists to say the value is unwanted.
        assert!(
            findings(indoc! {"
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
    fn an_unread_loop_variable_is_not_reported() {
        // Idiomatic "repeat n times"; firing here would make the checker unusable.
        assert!(
            findings(indoc! {"
                total = 0
                for idx in 1..3
                    total += 1
                total
            "})
            .is_empty()
        );
    }

    #[test]
    fn a_global_read_from_a_function_is_not_reported() {
        // The entry cannot see the read, so globals are live wherever a graph ends.
        assert!(
            findings(indoc! {"
                g = 1
                read(): int is ::g
                read()
            "})
            .is_empty()
        );
    }

    #[test]
    fn a_global_written_in_a_function_is_not_a_dead_store() {
        assert!(
            function_findings(indoc! {"
                bump(): int is
                    ::g = ::g + 1
                    0
                g = 1
                bump()
            "})
            .is_empty()
        );
    }

    #[test]
    fn a_function_local_nothing_reads_is_reported() {
        assert_eq!(
            function_findings(indoc! {"
                f(n: int): int is
                    unused = n * 2
                    n
                f(1)
            "}),
            vec![("unused-binding", "unused".to_string())]
        );
    }

    #[test]
    fn a_function_result_counts_as_a_read() {
        // The body's trailing expression is what the function returns — the read that matters most.
        assert!(
            function_findings(indoc! {"
                f(n: int): int is
                    doubled = n * 2
                    doubled
                f(1)
            "})
            .is_empty()
        );
    }

    #[test]
    fn a_read_inside_an_if_expression_counts() {
        // The CFG treats a nested if-expression as atomic, so its reads must still be collected —
        // otherwise `a` would look dead.
        assert!(findings("a = 1\nb = a if a > 0 else 0\nb").is_empty());
    }

    #[test]
    fn a_read_through_interpolation_counts() {
        assert!(findings("name = 'x'\nf'hello {name}'").is_empty());
    }

    #[test]
    fn a_read_through_indexing_counts() {
        assert!(
            findings(indoc! {"
                idx = 1
                arr = [1, 2, 3]
                arr[idx]
            "})
            .is_empty()
        );
    }

    #[test]
    fn writing_one_element_is_not_a_whole_new_value() {
        // `arr[0] = 9` updates the array in place: it reads `arr` rather than replacing it, so the
        // original binding is not a dead store.
        assert!(
            findings(indoc! {"
                arr = [1, 2, 3]
                arr[0] = 9
                arr[0]
            "})
            .is_empty()
        );
    }

    #[test]
    fn both_findings_can_appear_in_one_run() {
        // The batch-reporting property: fail-fast sema could never show this.
        let found = findings(indoc! {"
            dead = 1
            dead = 2
            unread = 3
            dead
        "});
        let codes: Vec<&str> = found.iter().map(|(code, _)| *code).collect();
        assert!(codes.contains(&DiagnosticCode::DeadStore.name()));
        assert!(codes.contains(&DiagnosticCode::UnusedBinding.name()));
        assert_eq!(found.len(), 2);
    }
}
