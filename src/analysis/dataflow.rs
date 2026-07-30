// SPDX-License-Identifier: MIT

//! The dataflow framework: everything an analysis needs except the analysis itself.
//!
//! A *dataflow analysis* answers a question about every point in a program by propagating **facts**
//! along the control-flow graph until they stop changing. Three pieces define one:
//!
//! * a [`MergeableFact`] — what the analysis knows at a point, with a "nothing known yet" value and
//!   a merge for what two incoming paths knew;
//! * a [`Direction`] — forward along the edges (what has happened) or backward against them (what
//!   is still going to happen);
//! * a **transfer function** saying how one action changes a fact.
//!
//! [`solve`] then does the rest: start every block knowing nothing, put the boundary fact where
//! execution begins (or ends), and keep re-evaluating blocks whose input changed until nothing
//! does. That terminates because merging only ever *adds* information and there is a limit to how
//! much can be added — an obligation on whoever writes the next fact type, not something the
//! framework can check.

use rustc_hash::FxHashSet;

use crate::analysis::cfg::{Action, BlockId, Cfg, Terminator};
use crate::parser::SymId;

/// What [`solve`] needs from a fact type — and the whole of what it needs.
///
/// A *fact* is whatever an analysis knows at one point in the program: "execution can reach here",
/// "these bindings are still going to be read". Facts arrive at a point from several paths at once,
/// so the solver requires exactly two things and no others: a value for a point it has not examined
/// yet, and a way to combine what two paths knew that also says whether it learned anything.
///
/// That pair is why this is a trait rather than a couple of methods on each fact type — one
/// worklist solver then serves every analysis. Implementing this and [`Analysis`] is the entire
/// cost of adding a new one.
///
/// Compiler literature calls this structure a *join-semilattice with a least element*: facts
/// ordered by how much they say, where [`nothing_known`](Self::nothing_known) is the "bottom" and
/// [`merge`](Self::merge) is the "join". Those are the words to search for; the methods are spelled
/// here as what they do.
pub trait MergeableFact: Clone + PartialEq {
    /// What is known about a point the solver has not examined yet: nothing.
    ///
    /// [`solve`] starts every block here, so merging it into another fact must add nothing.
    fn nothing_known() -> Self;

    /// Combines another path's fact into this one, returning whether this fact gained anything.
    ///
    /// [`solve`] calls this once per incoming edge, and the returned flag is its termination
    /// signal: a block whose fact stopped growing does not re-queue its neighbours.
    ///
    /// Merging must only ever *add* information, never remove it, and there must be a limit to how
    /// much can be added — otherwise the facts never settle. Neither property can be checked here;
    /// a `merge` that reports a gain for ever is what [`solve`]'s visit budget turns into a failed
    /// assertion rather than a hung process.
    fn merge(&mut self, other: &Self) -> bool;
}

/// Whether facts flow along the edges or against them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// From entry towards exit: what is true about how a point was reached.
    Forward,
    /// From exit back towards entry: what is true about what happens after a point.
    Backward,
}

/// One analysis: a lattice, a direction, and the transfer functions over a block's contents.
pub trait Analysis<'ast> {
    type Fact: MergeableFact;

    const DIRECTION: Direction;

    /// The fact where the analysis starts — at the entry block for a forward analysis, at the exit
    /// for a backward one.
    fn boundary(&self) -> Self::Fact;

    /// How one action transforms a fact. For a backward analysis this is applied against program
    /// order, so it describes the same action seen from the other side.
    fn transfer_action(&self, action: &Action<'ast>, fact: &mut Self::Fact);

    /// How a block's terminator transforms a fact — a `Branch` reads its condition, a `Return` its
    /// result expression.
    fn transfer_terminator(&self, terminator: &Terminator, fact: &mut Self::Fact);

    /// Which of `block_id`'s successors control can actually take.
    ///
    /// Every one of them, unless an analysis knows better: an analysis that can decide a condition
    /// before the program runs prunes the edge that is never taken, which is what makes anything
    /// unreachable at all. Whether an edge is feasible is an analysis's judgement rather than the
    /// graph's, so it lives here — the graph records what the program *says*.
    ///
    /// [`solve`] honours this in both directions: an edge dropped here carries no facts either way.
    fn live_successors(&self, cfg: &Cfg<'ast>, block_id: BlockId) -> Vec<BlockId> {
        cfg.successors(block_id)
    }
}

/// The facts either side of every block, in **program order**: `before` holds just before a block's
/// actions run and `after` just after its terminator, whichever direction produced them.
#[derive(Debug, Clone)]
pub struct Solution<F> {
    before: Vec<F>,
    after: Vec<F>,
}

impl<F> Solution<F> {
    /// The fact holding immediately before `block_id`'s first action.
    pub fn before(&self, block_id: BlockId) -> &F {
        &self.before[block_id.value()]
    }

    /// The fact holding immediately after `block_id`'s terminator.
    pub fn after(&self, block_id: BlockId) -> &F {
        &self.after[block_id.value()]
    }
}

/// How many block visits `solve` allows before it decides the analysis will never converge.
///
/// A correct analysis over a lattice of finite height needs a small multiple of the block count. A
/// bound turns the one bug this framework cannot rule out — a non-monotone `join` — into a loud
/// failure instead of a hung process.
const VISIT_BUDGET_PER_BLOCK: usize = 64;

/// Runs `analysis` over `cfg` until the facts stop changing.
///
/// # Panics
///
/// If the analysis has not converged after [`VISIT_BUDGET_PER_BLOCK`] visits per block, which means
/// its `join` is not monotone or its lattice has no finite height.
pub fn solve<'ast, A: Analysis<'ast>>(cfg: &Cfg<'ast>, analysis: &A) -> Solution<A::Fact> {
    let block_count = cfg.blocks().len();
    let mut before = vec![A::Fact::nothing_known(); block_count];
    let mut after = vec![A::Fact::nothing_known(); block_count];

    // Every block starts on the worklist. Seeding only the boundary would stall immediately: the
    // first evaluation of a block whose neighbours are all still `bottom` produces `bottom` too,
    // sees no change, and so never enqueues anyone. The boundary fact enters through the join
    // below, on whichever block owns it.
    let mut worklist: Vec<BlockId> = cfg.block_ids().collect();
    if A::DIRECTION == Direction::Forward {
        // `pop` takes from the end, so reversing makes the entry block the first one evaluated —
        // facts then flow with the edges instead of against them, and convergence takes fewer
        // passes. Correctness does not depend on the order.
        worklist.reverse();
    }

    let budget = block_count * VISIT_BUDGET_PER_BLOCK + 1;
    let mut visits = 0;

    while let Some(block_id) = worklist.pop() {
        visits += 1;
        assert!(
            visits <= budget,
            "dataflow analysis did not converge after {visits} block visits — its `join` is \
             probably not monotone"
        );

        match A::DIRECTION {
            Direction::Forward => {
                // Input is everything the predecessors produced — over the edges that can be
                // taken, so a predecessor whose branch never comes this way contributes nothing.
                let mut incoming = A::Fact::nothing_known();
                for predecessor in cfg.predecessors(block_id) {
                    if analysis
                        .live_successors(cfg, *predecessor)
                        .contains(&block_id)
                    {
                        incoming.merge(&after[predecessor.value()]);
                    }
                }
                if block_id == cfg.entry() {
                    incoming.merge(&analysis.boundary());
                }
                before[block_id.value()] = incoming;

                let mut fact = before[block_id.value()].clone();
                for action in &cfg.block(block_id).actions {
                    analysis.transfer_action(action, &mut fact);
                }
                analysis.transfer_terminator(&cfg.block(block_id).terminator, &mut fact);

                if fact != after[block_id.value()] {
                    after[block_id.value()] = fact;
                    worklist.extend(analysis.live_successors(cfg, block_id));
                }
            }
            Direction::Backward => {
                // Input is everything the reachable successors will need.
                let mut incoming = A::Fact::nothing_known();
                for successor in analysis.live_successors(cfg, block_id) {
                    incoming.merge(&before[successor.value()]);
                }
                if block_id == cfg.exit() {
                    incoming.merge(&analysis.boundary());
                }
                after[block_id.value()] = incoming;

                let mut fact = after[block_id.value()].clone();
                analysis.transfer_terminator(&cfg.block(block_id).terminator, &mut fact);
                for action in cfg.block(block_id).actions.iter().rev() {
                    analysis.transfer_action(action, &mut fact);
                }

                if fact != before[block_id.value()] {
                    before[block_id.value()] = fact;
                    worklist.extend_from_slice(cfg.predecessors(block_id));
                }
            }
        }
    }

    Solution { before, after }
}

/// Whether execution can arrive at a program point.
///
/// `false` is "not reached yet" and joining is "reached from anywhere", so a point stays false only
/// if no path whatsoever leads to it — which is exactly what makes it unreachable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReachabilityFact(bool);

impl ReachabilityFact {
    pub fn reached() -> ReachabilityFact {
        ReachabilityFact(true)
    }

    pub fn is_reached(self) -> bool {
        self.0
    }
}

impl MergeableFact for ReachabilityFact {
    fn nothing_known() -> ReachabilityFact {
        ReachabilityFact(false)
    }

    fn merge(&mut self, other: &ReachabilityFact) -> bool {
        let joined = self.0 || other.0;
        let changed = joined != self.0;
        self.0 = joined;
        changed
    }
}

/// A set of symbols: the fact shape of any analysis asking "which bindings satisfy X *here*".
///
/// Liveness is the first user — the symbols still to be read — and definite assignment and
/// ownership want the same shape. Joining is union, so a fact only ever grows and the lattice
/// height is bounded by the number of symbols in the program, which is what makes [`solve`]
/// terminate.
///
/// A dedicated bitset would make `join` a word-wise OR and get the changed flag out of the same
/// loop; the sets here are small enough that it would be a refinement with no behavioural
/// difference.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SymbolSetFact(FxHashSet<SymId>);

impl SymbolSetFact {
    pub fn contains(&self, sym_id: SymId) -> bool {
        self.0.contains(&sym_id)
    }

    /// Adds a symbol, returning whether it was absent.
    pub fn insert(&mut self, sym_id: SymId) -> bool {
        self.0.insert(sym_id)
    }

    /// Drops a symbol, returning whether it was present.
    pub fn remove(&mut self, sym_id: SymId) -> bool {
        self.0.remove(&sym_id)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = SymId> + '_ {
        self.0.iter().copied()
    }
}

impl MergeableFact for SymbolSetFact {
    fn nothing_known() -> SymbolSetFact {
        SymbolSetFact(FxHashSet::default())
    }

    fn merge(&mut self, other: &SymbolSetFact) -> bool {
        let before = self.0.len();
        self.0.extend(other.0.iter().copied());
        self.0.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{ProgramAst, parse};
    use indoc::indoc;

    fn parsed(src: &str) -> ProgramAst<'_> {
        parse(src).unwrap()
    }

    /// A forward analysis that simply records how far execution can get: every block it reaches
    /// ends up `true`. Deliberately trivial, so a failure here is the solver's fault and not a
    /// checker's.
    struct Reached;

    impl<'ast> Analysis<'ast> for Reached {
        type Fact = ReachabilityFact;
        const DIRECTION: Direction = Direction::Forward;

        fn boundary(&self) -> ReachabilityFact {
            ReachabilityFact::reached()
        }

        fn transfer_action(&self, _action: &Action<'ast>, _fact: &mut ReachabilityFact) {}

        fn transfer_terminator(&self, _terminator: &Terminator, _fact: &mut ReachabilityFact) {}
    }

    /// A backward analysis collecting every symbol bound by a loop, to exercise the other
    /// direction without depending on the liveness checker.
    struct BoundSymbols;

    impl<'ast> Analysis<'ast> for BoundSymbols {
        type Fact = SymbolSetFact;
        const DIRECTION: Direction = Direction::Backward;

        fn boundary(&self) -> SymbolSetFact {
            SymbolSetFact::nothing_known()
        }

        fn transfer_action(&self, action: &Action<'ast>, fact: &mut SymbolSetFact) {
            if let Action::Bind(sym_id) = action {
                fact.insert(*sym_id);
            }
        }

        fn transfer_terminator(&self, _terminator: &Terminator, _fact: &mut SymbolSetFact) {}
    }

    #[test]
    fn a_forward_analysis_reaches_every_block_of_straight_line_code() {
        let ast = parsed("a = 1\nb = 2\na + b");
        let cfg = Cfg::build_entry(&ast);
        let solution = solve(&cfg, &Reached);
        for block_id in cfg.block_ids() {
            assert!(
                solution.before(block_id).is_reached(),
                "block {block_id:?} not reached"
            );
        }
    }

    #[test]
    fn facts_propagate_around_a_back_edge() {
        // A loop needs more than one pass: the header is first evaluated before the body has
        // contributed anything, so the solver has to come back to it.
        let ast = parsed(indoc! {"
            n = 0
            while n < 3
                n += 1
            n
        "});
        let cfg = Cfg::build_entry(&ast);
        let solution = solve(&cfg, &Reached);
        for block_id in cfg.block_ids() {
            assert!(
                solution.before(block_id).is_reached(),
                "block {block_id:?} not reached"
            );
        }
    }

    #[test]
    fn a_backward_analysis_carries_facts_against_the_edges() {
        // The loop variable is bound inside the body, and a backward analysis must have that fact
        // available *before* the loop — which only happens by flowing against the edges.
        let ast = parsed(indoc! {"
            total = 0
            for idx in 1..3
                total += idx
            total
        "});
        let cfg = Cfg::build_entry(&ast);
        let solution = solve(&cfg, &BoundSymbols);
        assert_eq!(
            solution.before(cfg.entry()).len(),
            1,
            "the binding inside the loop reaches the entry"
        );
        assert!(
            solution.after(cfg.exit()).is_empty(),
            "nothing is bound after the program ends"
        );
    }

    #[test]
    fn a_single_block_program_converges() {
        let ast = parsed("1 + 1");
        let cfg = Cfg::build_entry(&ast);
        let solution = solve(&cfg, &Reached);
        assert!(solution.before(cfg.entry()).is_reached());
        assert!(solution.after(cfg.entry()).is_reached());
    }

    #[test]
    fn the_reachability_fact_reports_changes_exactly_once() {
        let mut fact = ReachabilityFact::nothing_known();
        assert!(!fact.is_reached());
        assert!(
            fact.merge(&ReachabilityFact::reached()),
            "unreached to reached is a change"
        );
        assert!(
            !fact.merge(&ReachabilityFact::reached()),
            "already reached is not a change"
        );
        assert!(
            !fact.merge(&ReachabilityFact::nothing_known()),
            "joining downward never changes anything"
        );
        assert!(fact.is_reached());
    }

    #[test]
    fn the_symbol_set_fact_reports_changes_exactly_once() {
        let mut fact = SymbolSetFact::nothing_known();
        let mut other = SymbolSetFact::nothing_known();
        other.insert(SymId(1));
        other.insert(SymId(2));
        assert!(fact.merge(&other), "two new symbols");
        assert!(!fact.merge(&other), "the same symbols again change nothing");
        assert_eq!(fact.len(), 2);
        assert!(fact.contains(SymId(1)));
        assert!(fact.remove(SymId(1)));
        assert!(!fact.contains(SymId(1)));
    }

    /// A lattice whose `join` always lands strictly above what arrived, so a cycle in the graph
    /// drives it up for ever. This is the mistake the visit budget exists to catch — an
    /// unbounded lattice, which no amount of iterating will settle.
    #[derive(Clone, PartialEq)]
    struct NeverSettles(u32);

    impl MergeableFact for NeverSettles {
        fn nothing_known() -> NeverSettles {
            NeverSettles(0)
        }

        fn merge(&mut self, other: &NeverSettles) -> bool {
            self.0 = self.0.max(other.0 + 1);
            true
        }
    }

    struct Divergent;

    impl<'ast> Analysis<'ast> for Divergent {
        type Fact = NeverSettles;
        const DIRECTION: Direction = Direction::Forward;

        fn boundary(&self) -> NeverSettles {
            NeverSettles(0)
        }

        fn transfer_action(&self, _action: &Action<'ast>, _fact: &mut NeverSettles) {}

        fn transfer_terminator(&self, _terminator: &Terminator, _fact: &mut NeverSettles) {}
    }

    #[test]
    #[should_panic(expected = "did not converge")]
    fn a_non_monotone_analysis_is_caught_rather_than_looping_forever() {
        let ast = parsed(indoc! {"
            n = 0
            while n < 3
                n += 1
            n
        "});
        let cfg = Cfg::build_entry(&ast);
        solve(&cfg, &Divergent);
    }
}
