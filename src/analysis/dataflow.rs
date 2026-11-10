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

use crate::ir::{BlockId, Flat, Function, NodeId, SideTable, Terminator};

/// What [`solve`] needs from a fact type — and the whole of what it needs.
///
/// A *fact* is whatever an analysis knows at one point in the program: "execution can reach here",
/// "these variables are still going to be read". Facts arrive at a point from several paths at once,
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
///
/// The contract, which the solver relies on and cannot check:
///
/// * `merge` is associative, commutative, and idempotent — the order in which paths are combined,
///   and combining one twice, make no difference;
/// * `nothing_known()` is its identity — merging it into a fact changes nothing;
/// * a fact can grow only a bounded number of times: [`Analysis::height`].
pub trait MergeableFact: Clone {
    /// What is known about a point the solver has not examined yet: nothing.
    fn nothing_known() -> Self;

    /// Combines another path's fact into this one, returning whether this fact gained anything.
    ///
    /// The returned flag is [`solve`]'s signal: a block whose fact did not grow does not re-queue
    /// its neighbours.
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
///
/// An analysis holds the function it analyses, so a transfer function can look at a statement's
/// operation and children. Transfer functions must be **monotone**: a fact that says more going in
/// never says less coming out. [`solve`] merges each new result into the block's fact, so a lapse
/// cannot make facts oscillate — but it can make them less precise than they should be.
pub trait Analysis {
    type Fact: MergeableFact;

    const DIRECTION: Direction;

    /// How many times a fact can grow in `function`, at most: the longest chain of ever-larger
    /// facts. One for a yes-or-no fact; the number of variables for a set of them. [`solve`] treats
    /// a block whose fact grows more often as a fact type without a bound, and stops.
    fn height(&self, function: &Function<Flat>) -> usize;

    /// The fact where the analysis starts — at the entry block for a forward analysis, after every
    /// returning block for a backward one. A block ending in a runtime error starts from nothing:
    /// the program ends there.
    fn boundary(&self) -> Self::Fact;

    /// How one statement transforms a fact. For a backward analysis this is applied against
    /// program order, so it describes the same statement seen from the other side.
    fn transfer_statement(&self, statement: NodeId, fact: &mut Self::Fact);

    /// How a block's terminator transforms a fact — a `Branch` reads its condition, a `Return` its
    /// result.
    fn transfer_terminator(&self, terminator: &Terminator, fact: &mut Self::Fact);

    /// Which of `block_id`'s successors control can actually take.
    ///
    /// Every one of them, unless an analysis knows better: an analysis that can decide a condition
    /// before the program runs prunes the edge that is never taken, which is what makes anything
    /// unreachable at all. Whether an edge is feasible is an analysis's judgement rather than the
    /// graph's, so it lives here — the graph records what the program *says*.
    ///
    /// [`solve`] honours this in both directions: an edge dropped here carries no facts either way.
    fn feasible_successors(&self, function: &Function<Flat>, block_id: BlockId) -> Vec<BlockId> {
        function.successors(block_id)
    }
}

/// The facts either side of every block, in **program order**: `before` holds just before a
/// block's statements run and `after` just after its terminator, whichever direction produced
/// them.
#[derive(Debug, Clone)]
pub struct Solution<F: Clone> {
    before: SideTable<BlockId, F>,
    after: SideTable<BlockId, F>,
}

impl<F: Clone> Solution<F> {
    /// The fact holding immediately before `block_id`'s first statement.
    pub fn before(&self, block_id: BlockId) -> &F {
        &self.before[block_id]
    }

    /// The fact holding immediately after `block_id`'s terminator.
    pub fn after(&self, block_id: BlockId) -> &F {
        &self.after[block_id]
    }
}

/// Runs `analysis` over `function` until the facts stop changing.
///
/// Each block's result is merged into its fact rather than replacing it, so facts only grow, and
/// a block is re-queued only when a neighbour's fact grew — at most [`Analysis::height`] times per
/// block, which is what bounds the work.
///
/// # Panics
///
/// If a block's fact grows more than [`Analysis::height`] times: the fact type has no bound, or the
/// analysis understates it. A loud failure instead of a hung process.
pub fn solve<A: Analysis>(function: &Function<Flat>, analysis: &A) -> Solution<A::Fact> {
    let blocks = &function.shape.blocks;
    let mut before = SideTable::new(A::Fact::nothing_known());
    let mut after = SideTable::new(A::Fact::nothing_known());
    let predecessors = function.predecessors();
    let height = analysis.height(function);
    let mut growths: SideTable<BlockId, usize> = SideTable::default();

    // Every block starts on the worklist. Seeding only the boundary would stall immediately: the
    // first evaluation of a block whose neighbours are all still "nothing known" produces that
    // too, sees no change, and so never enqueues anyone.
    let mut worklist = Worklist::default();
    let mut order: Vec<BlockId> = blocks.iter().map(|(block_id, _)| block_id).collect();
    if A::DIRECTION == Direction::Forward {
        // `pop` takes from the end, so reversing makes the entry block the first one evaluated —
        // facts then flow with the edges, and convergence takes fewer passes.
        order.reverse();
    }
    worklist.extend(order);

    while let Some(block_id) = worklist.pop() {
        let block = &blocks[block_id];
        let statements = function.arena.items(block.statements);

        let grew = match A::DIRECTION {
            Direction::Forward => {
                // Input is everything the predecessors produced — over the edges that can be
                // taken, so a predecessor whose branch never comes this way contributes nothing.
                let mut incoming = A::Fact::nothing_known();
                for &predecessor in &predecessors[block_id] {
                    if analysis
                        .feasible_successors(function, predecessor)
                        .contains(&block_id)
                    {
                        incoming.merge(&after[predecessor]);
                    }
                }
                if block_id == function.shape.entry {
                    incoming.merge(&analysis.boundary());
                }
                let mut fact = incoming.clone();
                before[block_id] = incoming;
                for &statement in statements {
                    analysis.transfer_statement(statement, &mut fact);
                }
                analysis.transfer_terminator(&block.terminator, &mut fact);
                let grew = after[block_id].merge(&fact);
                if grew {
                    worklist.extend(analysis.feasible_successors(function, block_id));
                }
                grew
            }
            Direction::Backward => {
                // Input is everything the reachable successors will need.
                let mut incoming = A::Fact::nothing_known();
                for successor in analysis.feasible_successors(function, block_id) {
                    incoming.merge(&before[successor]);
                }
                if matches!(block.terminator, Terminator::Return(_)) {
                    incoming.merge(&analysis.boundary());
                }
                let mut fact = incoming.clone();
                after[block_id] = incoming;
                analysis.transfer_terminator(&block.terminator, &mut fact);
                for &statement in statements.iter().rev() {
                    analysis.transfer_statement(statement, &mut fact);
                }
                let grew = before[block_id].merge(&fact);
                if grew {
                    worklist.extend(predecessors[block_id].iter().copied());
                }
                grew
            }
        };
        if grew {
            growths[block_id] += 1;
            assert!(
                growths[block_id] <= height,
                "dataflow analysis did not converge: a fact grew more than the analysis's height \
                 of {height} allows — the fact type is unbounded, or the height understated"
            );
        }
    }

    Solution { before, after }
}

/// Blocks waiting to be evaluated, each at most once at a time: a block queued twice would only be
/// evaluated twice for the same input.
#[derive(Default)]
struct Worklist {
    pending: Vec<BlockId>,
    queued: SideTable<BlockId, bool>,
}

impl Worklist {
    fn extend(&mut self, block_ids: impl IntoIterator<Item = BlockId>) {
        for block_id in block_ids {
            if !self.queued[block_id] {
                self.queued[block_id] = true;
                self.pending.push(block_id);
            }
        }
    }

    fn pop(&mut self) -> Option<BlockId> {
        let block_id = self.pending.pop()?;
        self.queued[block_id] = false;
        Some(block_id)
    }
}

/// Walks a solved backward analysis through `block_id` statement by statement, last first, calling
/// `visit` with each statement and the fact holding just *after* it — what a checker needs to judge
/// one statement, which the per-block solution does not show.
pub fn replay_backward<A: Analysis>(
    function: &Function<Flat>,
    analysis: &A,
    solution: &Solution<A::Fact>,
    block_id: BlockId,
    mut visit: impl FnMut(NodeId, &A::Fact),
) {
    let block = &function.shape.blocks[block_id];
    let mut fact = solution.after(block_id).clone();
    analysis.transfer_terminator(&block.terminator, &mut fact);
    for &statement in function.arena.items(block.statements).iter().rev() {
        visit(statement, &fact);
        analysis.transfer_statement(statement, &mut fact);
    }
}

/// Whether execution can reach a point: the fact of reachability. It can grow once, from not
/// reached to reached.
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

/// A set of variables, by index: the fact shape of any analysis asking "which variables satisfy X
/// *here*". Liveness is the first user — the variables still to be read. The analysis using it
/// numbers its variables densely from zero; each is one bit, so merging is a word-wise `or` and a
/// set can grow at most once per variable. Two sets with the same members are equal, however they
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

impl MergeableFact for VariableSet {
    fn nothing_known() -> VariableSet {
        VariableSet::default()
    }

    fn merge(&mut self, other: &VariableSet) -> bool {
        if self.words.len() < other.words.len() {
            self.words.resize(other.words.len(), 0);
        }
        let mut changed = false;
        for (word, &incoming) in self.words.iter_mut().zip(&other.words) {
            changed |= incoming & !*word != 0;
            *word |= incoming;
        }
        changed
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
        type Fact = ReachabilityFact;
        const DIRECTION: Direction = Direction::Forward;

        fn boundary(&self) -> ReachabilityFact {
            ReachabilityFact::reached()
        }

        fn height(&self, _function: &Function<Flat>) -> usize {
            1
        }

        fn transfer_statement(&self, _statement: NodeId, _fact: &mut ReachabilityFact) {}

        fn transfer_terminator(&self, _terminator: &Terminator, _fact: &mut ReachabilityFact) {}
    }

    /// Every statement's node id, collected backward: what is still going to be written.
    struct WrittenAhead<'function> {
        function: &'function Function<Flat>,
    }

    impl Analysis for WrittenAhead<'_> {
        type Fact = VariableSet;
        const DIRECTION: Direction = Direction::Backward;

        fn boundary(&self) -> VariableSet {
            VariableSet::nothing_known()
        }

        fn height(&self, _function: &Function<Flat>) -> usize {
            self.function.arena.locals.len()
        }

        fn transfer_statement(&self, statement: NodeId, fact: &mut VariableSet) {
            if let Operation::WriteLocal { local_id, .. } =
                self.function.arena.operations[statement]
            {
                fact.insert(local_id.index() as usize);
            }
        }

        fn transfer_terminator(&self, _terminator: &Terminator, _fact: &mut VariableSet) {}
    }

    /// Reachability that never takes a branch's `then` edge.
    struct ElseOnly;

    impl Analysis for ElseOnly {
        type Fact = ReachabilityFact;
        const DIRECTION: Direction = Direction::Forward;

        fn boundary(&self) -> ReachabilityFact {
            ReachabilityFact::reached()
        }

        fn height(&self, _function: &Function<Flat>) -> usize {
            1
        }

        fn transfer_statement(&self, _statement: NodeId, _fact: &mut ReachabilityFact) {}

        fn transfer_terminator(&self, _terminator: &Terminator, _fact: &mut ReachabilityFact) {}

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
    fn facts_propagate_around_a_back_edge() {
        every_block_reached(LOOP);
    }

    #[test]
    fn a_single_block_program_converges() {
        every_block_reached("1 + 1");
    }

    #[test]
    fn a_backward_analysis_carries_facts_against_the_edges() {
        // The loop body writes the index; a backward analysis must have that fact before the
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
    fn a_pruned_edge_carries_no_facts_forward() {
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
    fn replaying_a_block_backward_gives_the_fact_after_each_statement() {
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
    fn the_variable_set_reports_changes_exactly_once() {
        let mut fact = VariableSet::nothing_known();
        let mut other = VariableSet::nothing_known();
        assert!(other.insert(1));
        assert!(other.insert(2));
        assert!(!other.insert(2), "already there");
        assert!(fact.merge(&other), "two new variables");
        assert!(
            !fact.merge(&other),
            "the same variables again change nothing"
        );
        assert_eq!(fact.len(), 2);
        assert!(fact.contains(1));
        assert!(fact.remove(1));
        assert!(!fact.remove(1), "already gone");
        assert!(!fact.contains(1));
        assert_eq!(fact.iter().collect::<Vec<_>>(), [2]);
    }

    #[test]
    fn the_variable_set_spans_many_words() {
        let mut fact = VariableSet::nothing_known();
        for variable in [0, 63, 64, 65, 129, 1000] {
            assert!(fact.insert(variable));
        }
        assert!(!fact.contains(62) && !fact.contains(128) && !fact.contains(5000));
        assert_eq!(fact.iter().collect::<Vec<_>>(), [0, 63, 64, 65, 129, 1000]);
        let mut small = VariableSet::nothing_known();
        small.insert(3);
        assert!(small.merge(&fact), "a longer set merges into a shorter one");
        assert_eq!(small.len(), 7);
        assert!(fact.merge(&small), "only 3 is new to it");
        assert_eq!(fact, small);
    }

    #[test]
    fn sets_with_the_same_members_are_equal_whatever_their_history() {
        let mut grown = VariableSet::nothing_known();
        grown.insert(200);
        grown.remove(200);
        grown.insert(5);
        let mut plain = VariableSet::nothing_known();
        plain.insert(5);
        assert_eq!(
            grown, plain,
            "the solver compares facts to detect a fixpoint"
        );
    }

    /// A lattice whose `merge` always lands strictly above what arrived, so a cycle in the graph
    /// drives it up for ever — the mistake the visit budget exists to catch.
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

    impl Analysis for Divergent {
        type Fact = NeverSettles;
        const DIRECTION: Direction = Direction::Forward;

        fn boundary(&self) -> NeverSettles {
            NeverSettles(0)
        }

        fn height(&self, _function: &Function<Flat>) -> usize {
            8
        }

        fn transfer_statement(&self, _statement: NodeId, _fact: &mut NeverSettles) {}

        fn transfer_terminator(&self, _terminator: &Terminator, _fact: &mut NeverSettles) {}
    }

    #[test]
    #[should_panic(expected = "did not converge")]
    fn a_non_monotone_analysis_is_caught_rather_than_looping_forever() {
        let function = flat_entry(LOOP);
        solve(&function, &Divergent);
    }

    /// A fact that can grow a thousand times: a count, merged by `max`.
    #[derive(Clone, PartialEq, Debug)]
    struct Count(u32);

    const CAP: u32 = 1000;

    impl MergeableFact for Count {
        fn nothing_known() -> Count {
            Count(0)
        }

        fn merge(&mut self, other: &Count) -> bool {
            let changed = other.0 > self.0;
            self.0 = self.0.max(other.0);
            changed
        }
    }

    /// Every statement adds one, up to `CAP`: monotone, finite, and slow to settle around a loop.
    struct Counting;

    impl Analysis for Counting {
        type Fact = Count;
        const DIRECTION: Direction = Direction::Forward;

        fn boundary(&self) -> Count {
            Count(0)
        }

        fn height(&self, _function: &Function<Flat>) -> usize {
            CAP as usize
        }

        fn transfer_statement(&self, _statement: NodeId, fact: &mut Count) {
            fact.0 = (fact.0 + 1).min(CAP);
        }

        fn transfer_terminator(&self, _terminator: &Terminator, _fact: &mut Count) {}
    }

    #[test]
    fn a_tall_but_finite_lattice_converges() {
        // The review's case: a correct analysis whose facts grow many times, which a visit budget
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
