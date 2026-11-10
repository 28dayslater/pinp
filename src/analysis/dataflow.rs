// SPDX-License-Identifier: MIT

// TODO: humanize the AI generated prose.

//! The dataflow framework: everything an analysis needs except the analysis itself.
//!
//! A *dataflow analysis* proves something about every **program point** — the gap between two
//! statements — by propagating what it knows along the control-flow graph until nothing changes.
//! Three pieces define one:
//!
//! * a [`Lattice`] — the values an analysis can know at a point, with a *bottom* that knows
//!   nothing and a *join* that combines what two incoming paths knew;
//! * a [`Direction`] — forward along the edges (what has happened) or backward against them (what
//!   is still going to happen);
//! * **transfer functions** saying how one statement turns the state before it into the state
//!   after it.
//!
//! [`solve`] then does the rest: start every block at bottom, put the boundary state where
//! execution begins (or ends), and keep re-evaluating blocks whose input changed until a
//! **fixpoint**, where nothing does. That terminates when the lattice has a finite height and the
//! transfer functions are monotone — obligations on whoever writes the next analysis, not
//! something the framework can check.
//!
//! The vocabulary follows clang's dataflow framework (`clang/Analysis/FlowSensitive`) and its
//! *Data flow analysis: an informal introduction*, which is the place to read further.

use crate::ir::{BlockId, Flat, Function, NodeId, SideTable, Terminator};

/// Whether a lattice operation changed the element it was applied to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LatticeEffect {
    Unchanged,
    Changed,
}

/// A *bounded join-semilattice*: what [`solve`] needs from an analysis's state, and the whole of
/// what it needs.
///
/// An element is whatever an analysis knows at one program point: "execution can reach here",
/// "these variables are still going to be read". Elements are ordered by how much they say.
/// States arrive at a point from several paths at once, so the solver needs exactly two things: an
/// element for a point it has not examined yet, and a way to combine what two paths knew that also
/// reports whether anything was learned. One worklist solver then serves every analysis.
///
/// The contract, which the solver relies on and cannot check:
///
/// * `join` is associative, commutative, and idempotent — the order in which paths are combined,
///   and combining one twice, make no difference;
/// * `bottom()` is its identity — joining it into an element changes nothing;
/// * an element can grow only a bounded number of times: [`Analysis::height`].
pub trait Lattice: Clone {
    /// The least element, ⊥: what is known about a point the solver has not examined yet — nothing.
    fn bottom() -> Self;

    /// Replaces this element with the least upper bound of it and `other`, reporting whether that
    /// changed it.
    ///
    /// The effect is [`solve`]'s signal: a block whose state did not change does not re-queue its
    /// neighbours.
    fn join(&mut self, other: &Self) -> LatticeEffect;
}

/// Whether states flow along the edges or against them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// From entry towards exit: what is true about how a point was reached.
    Forward,
    /// From exit back towards entry: what is true about what happens after a point.
    Backward,
}

/// One analysis: a lattice, a direction, and the transfer functions over a block's contents.
///
/// An analysis holds the function it analyses, so a transfer function can look at a statement's
/// operation and children. Transfer functions must be **monotone**: a state that says more going in
/// never says less coming out. [`solve`] joins each new result into the block's state, so a lapse
/// cannot make states oscillate — but it can make them less precise than they should be.
pub trait Analysis {
    type Lattice: Lattice;

    const DIRECTION: Direction;

    /// The lattice's height in `function`: the longest chain of ever-larger elements, and so how
    /// many times a state can grow. One for a yes-or-no lattice; the number of variables for a set
    /// of them. [`solve`] treats a block whose state grows more often as a lattice without a bound,
    /// and stops.
    fn height(&self, function: &Function<Flat>) -> usize;

    /// The state where the analysis starts — at the entry block for a forward analysis, after every
    /// returning block for a backward one. A block ending in a runtime error starts from bottom:
    /// the program ends there.
    fn boundary(&self) -> Self::Lattice;

    /// The transfer function of one statement. For a backward analysis it is applied against
    /// program order, so it describes the same statement seen from the other side.
    fn transfer_statement(&self, statement: NodeId, state: &mut Self::Lattice);

    /// The transfer function of a block's terminator — a `Branch` reads its condition, a `Return`
    /// its result.
    fn transfer_terminator(&self, terminator: &Terminator, state: &mut Self::Lattice);

    /// Which of `block_id`'s successors control can actually take.
    ///
    /// Every one of them, unless an analysis knows better: an analysis that can decide a condition
    /// before the program runs prunes the edge that is never taken, which is what makes anything
    /// unreachable at all. Whether an edge is feasible is an analysis's judgement rather than the
    /// graph's, so it lives here — the graph records what the program *says*.
    ///
    /// [`solve`] honours this in both directions: an edge dropped here carries no state either way.
    fn feasible_successors(&self, function: &Function<Flat>, block_id: BlockId) -> Vec<BlockId> {
        function.successors(block_id)
    }
}

/// The states either side of every block, in **program order**: `before` holds just before a
/// block's statements run and `after` just after its terminator, whichever direction produced
/// them.
#[derive(Debug, Clone)]
pub struct Solution<L: Clone> {
    before: SideTable<BlockId, L>,
    after: SideTable<BlockId, L>,
}

impl<L: Clone> Solution<L> {
    /// The state immediately before `block_id`'s first statement.
    pub fn before(&self, block_id: BlockId) -> &L {
        &self.before[block_id]
    }

    /// The state immediately after `block_id`'s terminator.
    pub fn after(&self, block_id: BlockId) -> &L {
        &self.after[block_id]
    }
}

/// Runs `analysis` over `function` to a fixpoint.
///
/// Each block's result is joined into its state rather than replacing it, so states only grow, and
/// a block is re-queued only when a neighbour's state changed — at most [`Analysis::height`] times
/// per block, which is what bounds the work.
///
/// # Panics
///
/// If a block's state grows more than [`Analysis::height`] times: the lattice has no bound, or the
/// analysis understates it. A loud failure instead of a hung process.
pub fn solve<A: Analysis>(function: &Function<Flat>, analysis: &A) -> Solution<A::Lattice> {
    let blocks = &function.shape.blocks;
    let mut before = SideTable::new(A::Lattice::bottom());
    let mut after = SideTable::new(A::Lattice::bottom());
    let predecessors = function.predecessors();
    let height = analysis.height(function);
    let mut growths: SideTable<BlockId, usize> = SideTable::default();

    // Every block starts on the worklist. Seeding only the boundary would stall immediately: the
    // first evaluation of a block whose neighbours are all still at bottom produces bottom too,
    // sees no change, and so never enqueues anyone.
    let mut worklist = Worklist::default();
    let mut order: Vec<BlockId> = blocks.iter().map(|(block_id, _)| block_id).collect();
    if A::DIRECTION == Direction::Forward {
        // `dequeue` takes from the end, so reversing makes the entry block the first one
        // evaluated — states then flow with the edges, and convergence takes fewer passes.
        order.reverse();
    }
    worklist.enqueue(order);

    while let Some(block_id) = worklist.dequeue() {
        let block = &blocks[block_id];
        let statements = function.arena.items(block.statements);

        let effect = match A::DIRECTION {
            Direction::Forward => {
                // Input is everything the predecessors produced — over the edges that can be
                // taken, so a predecessor whose branch never comes this way contributes nothing.
                let mut incoming = A::Lattice::bottom();
                for &predecessor in &predecessors[block_id] {
                    if analysis
                        .feasible_successors(function, predecessor)
                        .contains(&block_id)
                    {
                        incoming.join(&after[predecessor]);
                    }
                }
                if block_id == function.shape.entry {
                    incoming.join(&analysis.boundary());
                }
                let mut state = incoming.clone();
                before[block_id] = incoming;
                for &statement in statements {
                    analysis.transfer_statement(statement, &mut state);
                }
                analysis.transfer_terminator(&block.terminator, &mut state);
                let effect = after[block_id].join(&state);
                if effect == LatticeEffect::Changed {
                    worklist.enqueue(analysis.feasible_successors(function, block_id));
                }
                effect
            }
            Direction::Backward => {
                // Input is everything the reachable successors will need.
                let mut incoming = A::Lattice::bottom();
                for successor in analysis.feasible_successors(function, block_id) {
                    incoming.join(&before[successor]);
                }
                if matches!(block.terminator, Terminator::Return(_)) {
                    incoming.join(&analysis.boundary());
                }
                let mut state = incoming.clone();
                after[block_id] = incoming;
                analysis.transfer_terminator(&block.terminator, &mut state);
                for &statement in statements.iter().rev() {
                    analysis.transfer_statement(statement, &mut state);
                }
                let effect = before[block_id].join(&state);
                if effect == LatticeEffect::Changed {
                    worklist.enqueue(predecessors[block_id].iter().copied());
                }
                effect
            }
        };
        if effect == LatticeEffect::Changed {
            growths[block_id] += 1;
            assert!(
                growths[block_id] <= height,
                "dataflow analysis did not converge: a state grew more than the analysis's height \
                 of {height} allows — the lattice is unbounded, or the height understated"
            );
        }
    }

    Solution { before, after }
}

/// Blocks waiting to be evaluated, each at most once at a time: a block enqueued twice would only
/// be evaluated twice for the same input.
#[derive(Default)]
struct Worklist {
    pending: Vec<BlockId>,
    enqueued: SideTable<BlockId, bool>,
}

impl Worklist {
    fn enqueue(&mut self, block_ids: impl IntoIterator<Item = BlockId>) {
        for block_id in block_ids {
            if !self.enqueued[block_id] {
                self.enqueued[block_id] = true;
                self.pending.push(block_id);
            }
        }
    }

    fn dequeue(&mut self) -> Option<BlockId> {
        let block_id = self.pending.pop()?;
        self.enqueued[block_id] = false;
        Some(block_id)
    }
}

/// Walks a solved backward analysis through `block_id` statement by statement, last first, calling
/// `visit` with each statement and the state just *after* it — what a checker needs to judge
/// one statement, which the per-block solution does not show.
pub fn replay_backward<A: Analysis>(
    function: &Function<Flat>,
    analysis: &A,
    solution: &Solution<A::Lattice>,
    block_id: BlockId,
    mut visit: impl FnMut(NodeId, &A::Lattice),
) {
    let block = &function.shape.blocks[block_id];
    let mut state = solution.after(block_id).clone();
    analysis.transfer_terminator(&block.terminator, &mut state);
    for &statement in function.arena.items(block.statements).iter().rev() {
        visit(statement, &state);
        analysis.transfer_statement(statement, &mut state);
    }
}

/// Whether execution can reach a point: a two-element lattice, unreached (bottom) below reached. It
/// can grow once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReachabilityLattice(bool);

impl ReachabilityLattice {
    pub fn reached() -> ReachabilityLattice {
        ReachabilityLattice(true)
    }

    pub fn is_reached(self) -> bool {
        self.0
    }
}

impl Lattice for ReachabilityLattice {
    fn bottom() -> ReachabilityLattice {
        ReachabilityLattice(false)
    }

    fn join(&mut self, other: &ReachabilityLattice) -> LatticeEffect {
        let joined = self.0 || other.0;
        let effect = effect_of(joined != self.0);
        self.0 = joined;
        effect
    }
}

/// A set of variables, by index: the lattice of any analysis asking "which variables satisfy X
/// *here*", ordered by inclusion with the empty set as bottom. Liveness is the first user — the
/// variables still to be read. The analysis using it numbers its variables densely from zero; each
/// is one bit, so joining is a word-wise `or` and a set can grow at most once per variable. Two sets with the same members are equal, however they
/// grew.
#[derive(Debug, Clone, Default)]
pub struct VariableSet {
    words: Vec<u64>,
}

impl VariableSet {
    pub fn contains(&self, variable: usize) -> bool {
        self.words
            .get(variable / 64)
            .is_some_and(|word| word & (1 << (variable % 64)) != 0)
    }

    /// Adds `variable`, returning whether it was new.
    pub fn insert(&mut self, variable: usize) -> bool {
        if self.words.len() <= variable / 64 {
            self.words.resize(variable / 64 + 1, 0);
        }
        let word = &mut self.words[variable / 64];
        let bit = 1 << (variable % 64);
        let new = *word & bit == 0;
        *word |= bit;
        new
    }

    /// Removes `variable`, returning whether it was there.
    pub fn remove(&mut self, variable: usize) -> bool {
        let was_there = self.contains(variable);
        if was_there {
            self.words[variable / 64] &= !(1 << (variable % 64));
        }
        was_there
    }

    pub fn len(&self) -> usize {
        self.words
            .iter()
            .map(|word| word.count_ones() as usize)
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.words.iter().all(|&word| word == 0)
    }

    /// The members, in increasing order.
    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.words.iter().enumerate().flat_map(|(index, &word)| {
            (0..64)
                .filter(move |bit| word & (1 << bit) != 0)
                .map(move |bit| index * 64 + bit)
        })
    }
}

impl PartialEq for VariableSet {
    fn eq(&self, other: &VariableSet) -> bool {
        let longest = self.words.len().max(other.words.len());
        (0..longest).all(|index| {
            self.words.get(index).copied().unwrap_or(0)
                == other.words.get(index).copied().unwrap_or(0)
        })
    }
}

impl Lattice for VariableSet {
    fn bottom() -> VariableSet {
        VariableSet::default()
    }

    fn join(&mut self, other: &VariableSet) -> LatticeEffect {
        if self.words.len() < other.words.len() {
            self.words.resize(other.words.len(), 0);
        }
        let mut changed = false;
        for (word, &incoming) in self.words.iter_mut().zip(&other.words) {
            changed |= incoming & !*word != 0;
            *word |= incoming;
        }
        effect_of(changed)
    }
}

fn effect_of(changed: bool) -> LatticeEffect {
    match changed {
        true => LatticeEffect::Changed,
        false => LatticeEffect::Unchanged,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Function, Operation};
    use indoc::indoc;

    /// Every function of `src`, desugared and flattened; the entry is the last.
    fn flat_functions(src: &str) -> Vec<Function<Flat>> {
        let mut ast = crate::parser::parse(src).unwrap();
        crate::sema::analyze(&mut ast).unwrap();
        let program = crate::flatten::flatten_program(crate::desugar::desugar(&ast));
        program.functions.into_items().collect()
    }

    fn flat_entry(src: &str) -> Function<Flat> {
        flat_functions(src).pop().unwrap()
    }

    struct Reached;

    impl Analysis for Reached {
        type Lattice = ReachabilityLattice;
        const DIRECTION: Direction = Direction::Forward;

        fn boundary(&self) -> ReachabilityLattice {
            ReachabilityLattice::reached()
        }

        fn height(&self, _function: &Function<Flat>) -> usize {
            1
        }

        fn transfer_statement(&self, _statement: NodeId, _state: &mut ReachabilityLattice) {}

        fn transfer_terminator(&self, _terminator: &Terminator, _state: &mut ReachabilityLattice) {}
    }

    /// Every statement's node id, collected backward: what is still going to be written.
    struct WrittenAhead<'function> {
        function: &'function Function<Flat>,
    }

    impl Analysis for WrittenAhead<'_> {
        type Lattice = VariableSet;
        const DIRECTION: Direction = Direction::Backward;

        fn boundary(&self) -> VariableSet {
            VariableSet::bottom()
        }

        fn height(&self, _function: &Function<Flat>) -> usize {
            self.function.arena.locals.len()
        }

        fn transfer_statement(&self, statement: NodeId, state: &mut VariableSet) {
            if let Operation::WriteLocal { local_id, .. } =
                self.function.arena.operations[statement]
            {
                state.insert(local_id.index() as usize);
            }
        }

        fn transfer_terminator(&self, _terminator: &Terminator, _state: &mut VariableSet) {}
    }

    /// Reachability that never takes a branch's `then` edge.
    struct ElseOnly;

    impl Analysis for ElseOnly {
        type Lattice = ReachabilityLattice;
        const DIRECTION: Direction = Direction::Forward;

        fn boundary(&self) -> ReachabilityLattice {
            ReachabilityLattice::reached()
        }

        fn height(&self, _function: &Function<Flat>) -> usize {
            1
        }

        fn transfer_statement(&self, _statement: NodeId, _state: &mut ReachabilityLattice) {}

        fn transfer_terminator(&self, _terminator: &Terminator, _state: &mut ReachabilityLattice) {}

        fn feasible_successors(
            &self,
            function: &Function<Flat>,
            block_id: BlockId,
        ) -> Vec<BlockId> {
            match function.shape.blocks[block_id].terminator {
                Terminator::Branch { else_block, .. } => vec![else_block],
                _ => function.successors(block_id),
            }
        }
    }

    const LOOP: &str = indoc! {"
        n = 0
        while n < 3
            n += 1
        n
    "};

    fn every_block_reached(src: &str) {
        let function = flat_entry(src);
        let solution = solve(&function, &Reached);
        for (block_id, _) in function.shape.blocks.iter() {
            assert!(
                solution.before(block_id).is_reached(),
                "{block_id:?} not reached"
            );
            assert!(
                solution.after(block_id).is_reached(),
                "{block_id:?} not reached"
            );
        }
    }

    #[test]
    fn a_forward_analysis_reaches_every_block_of_straight_line_code() {
        every_block_reached("a = 1\nb = 2\na + b");
    }

    #[test]
    fn states_propagate_around_a_back_edge() {
        every_block_reached(LOOP);
    }

    #[test]
    fn a_single_block_program_converges() {
        every_block_reached("1 + 1");
    }

    #[test]
    fn a_backward_analysis_carries_states_against_the_edges() {
        // The loop body writes the index; a backward analysis must have that state before the
        // loop, which only happens by flowing against the edges.
        let function = flat_entry(indoc! {"
            total = 0
            for idx in 1..3
                total += idx
            total
        "});
        let solution = solve(
            &function,
            &WrittenAhead {
                function: &function,
            },
        );
        let entry = function.shape.entry;
        assert!(
            !solution.before(entry).is_empty(),
            "the loop's writes reach the entry"
        );
        let returning = function
            .shape
            .blocks
            .iter()
            .find(|(_, block)| matches!(block.terminator, Terminator::Return(_)))
            .map(|(block_id, _)| block_id)
            .unwrap();
        assert!(
            solution.after(returning).is_empty(),
            "nothing is written after the end"
        );
    }

    #[test]
    fn a_pruned_edge_carries_no_state_forward() {
        let function = flat_entry("c = true\nx = 0\nif c\n    x = 1\nelse\n    x = 2\nx");
        let solution = solve(&function, &ElseOnly);
        let (then_block, else_block) = match function.shape.blocks[function.shape.entry].terminator
        {
            Terminator::Branch {
                then_block,
                else_block,
                ..
            } => (then_block, else_block),
            other => panic!("Expected a branch, got {other:?}."),
        };
        assert!(!solution.before(then_block).is_reached());
        assert!(solution.before(else_block).is_reached());
    }

    #[test]
    fn replaying_a_block_backward_gives_the_state_after_each_statement() {
        // In a function, so the writes are to locals.
        let function =
            flat_functions("f(): int is\n    a = 1\n    b = 2\n    c = 3\n    c\nf()").remove(0);
        let analysis = WrittenAhead {
            function: &function,
        };
        let solution = solve(&function, &analysis);
        let mut seen = Vec::new();
        replay_backward(
            &function,
            &analysis,
            &solution,
            function.shape.entry,
            |statement, after| {
                seen.push((statement, after.len()));
            },
        );
        let statements = function
            .arena
            .items(function.shape.blocks[function.shape.entry].statements);
        let expected: Vec<(NodeId, usize)> = statements
            .iter()
            .rev()
            .enumerate()
            .map(|(later_writes, &statement)| (statement, later_writes))
            .collect();
        assert_eq!(
            seen, expected,
            "last statement first; each sees only the writes after it"
        );
    }

    #[test]
    fn the_reachability_lattice_reports_changes_exactly_once() {
        let mut state = ReachabilityLattice::bottom();
        assert!(!state.is_reached());
        assert_eq!(
            state.join(&ReachabilityLattice::reached()),
            LatticeEffect::Changed,
            "unreached to reached is a change"
        );
        assert_eq!(
            state.join(&ReachabilityLattice::reached()),
            LatticeEffect::Unchanged,
            "already reached is not a change"
        );
        assert_eq!(
            state.join(&ReachabilityLattice::bottom()),
            LatticeEffect::Unchanged,
            "joining bottom never changes anything"
        );
        assert!(state.is_reached());
    }

    #[test]
    fn the_variable_set_reports_changes_exactly_once() {
        let mut state = VariableSet::bottom();
        let mut other = VariableSet::bottom();
        assert!(other.insert(1));
        assert!(other.insert(2));
        assert!(!other.insert(2), "already there");
        assert_eq!(
            state.join(&other),
            LatticeEffect::Changed,
            "two new variables"
        );
        assert_eq!(
            state.join(&other),
            LatticeEffect::Unchanged,
            "the same variables again change nothing"
        );
        assert_eq!(state.len(), 2);
        assert!(state.contains(1));
        assert!(state.remove(1));
        assert!(!state.remove(1), "already gone");
        assert!(!state.contains(1));
        assert_eq!(state.iter().collect::<Vec<_>>(), [2]);
    }

    #[test]
    fn the_variable_set_spans_many_words() {
        let mut state = VariableSet::bottom();
        for variable in [0, 63, 64, 65, 129, 1000] {
            assert!(state.insert(variable));
        }
        assert!(!state.contains(62) && !state.contains(128) && !state.contains(5000));
        assert_eq!(state.iter().collect::<Vec<_>>(), [0, 63, 64, 65, 129, 1000]);
        let mut small = VariableSet::bottom();
        small.insert(3);
        assert_eq!(
            small.join(&state),
            LatticeEffect::Changed,
            "a longer set joins into a shorter one"
        );
        assert_eq!(small.len(), 7);
        assert_eq!(
            state.join(&small),
            LatticeEffect::Changed,
            "only 3 is new to it"
        );
        assert_eq!(state, small);
    }

    #[test]
    fn sets_with_the_same_members_are_equal_whatever_their_history() {
        let mut grown = VariableSet::bottom();
        grown.insert(200);
        grown.remove(200);
        grown.insert(5);
        let mut plain = VariableSet::bottom();
        plain.insert(5);
        assert_eq!(grown, plain, "a set is a lattice element, not a history");
    }

    /// A broken lattice whose `join` always lands strictly above what arrived, so a cycle in the
    /// graph drives it up for ever — the mistake the height check exists to catch.
    #[derive(Clone, PartialEq)]
    struct NeverSettles(u32);

    impl Lattice for NeverSettles {
        fn bottom() -> NeverSettles {
            NeverSettles(0)
        }

        fn join(&mut self, other: &NeverSettles) -> LatticeEffect {
            self.0 = self.0.max(other.0 + 1);
            LatticeEffect::Changed
        }
    }

    struct Divergent;

    impl Analysis for Divergent {
        type Lattice = NeverSettles;
        const DIRECTION: Direction = Direction::Forward;

        fn boundary(&self) -> NeverSettles {
            NeverSettles(0)
        }

        fn height(&self, _function: &Function<Flat>) -> usize {
            8
        }

        fn transfer_statement(&self, _statement: NodeId, _state: &mut NeverSettles) {}

        fn transfer_terminator(&self, _terminator: &Terminator, _state: &mut NeverSettles) {}
    }

    #[test]
    #[should_panic(expected = "did not converge")]
    fn a_non_monotone_analysis_is_caught_rather_than_looping_forever() {
        let function = flat_entry(LOOP);
        solve(&function, &Divergent);
    }

    /// A lattice a thousand elements tall: a count, joined by `max`.
    #[derive(Clone, PartialEq, Debug)]
    struct Count(u32);

    const CAP: u32 = 1000;

    impl Lattice for Count {
        fn bottom() -> Count {
            Count(0)
        }

        fn join(&mut self, other: &Count) -> LatticeEffect {
            let effect = effect_of(other.0 > self.0);
            self.0 = self.0.max(other.0);
            effect
        }
    }

    /// Every statement adds one, up to `CAP`: monotone, finite, and slow to settle around a loop.
    struct Counting;

    impl Analysis for Counting {
        type Lattice = Count;
        const DIRECTION: Direction = Direction::Forward;

        fn boundary(&self) -> Count {
            Count(0)
        }

        fn height(&self, _function: &Function<Flat>) -> usize {
            CAP as usize
        }

        fn transfer_statement(&self, _statement: NodeId, state: &mut Count) {
            state.0 = (state.0 + 1).min(CAP);
        }

        fn transfer_terminator(&self, _terminator: &Terminator, _state: &mut Count) {}
    }

    #[test]
    fn a_tall_but_finite_lattice_converges() {
        // The review's case: a correct analysis whose states grow many times, which a visit budget
        // proportional to the block count alone took for divergence.
        let function = flat_entry(LOOP);
        let solution = solve(&function, &Counting);
        assert_eq!(
            solution.after(function.shape.entry).0,
            1,
            "the entry runs once"
        );
        let (returning, _) = function
            .shape
            .blocks
            .iter()
            .find(|(_, block)| matches!(block.terminator, Terminator::Return(_)))
            .unwrap();
        assert_eq!(
            solution.before(returning).0,
            CAP,
            "the loop counts up to the cap"
        );
    }
}
