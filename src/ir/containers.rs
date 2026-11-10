// SPDX-License-Identifier: MIT

//! Typed ids and the containers the IR is built from: `IdVec` (owns items and creates their ids),
//! `SideTable` (per-id data with a default), and `List` handles into a shared `ListPool`.
//!
//! An id is a `u32` index wrapped in its own type, so a `LocalId` cannot be used where a `NodeId`
//! is expected. It is stored as `index + 1` in a `NonZeroU32`, which lets `Option<Id>` use the
//! forbidden zero for `None` and stay 4 bytes.

use std::fmt::Debug;
use std::hash::Hash;
use std::marker::PhantomData;
use std::ops::{Index, IndexMut};

/// A `u32` index with a type of its own. Implemented by [`typed_id!`].
///
/// # Safety
///
/// The implementing type must be `#[repr(transparent)]` over a `NonZeroU32` holding `index + 1`:
/// [`ListPool`] stores ids as that raw `u32` and hands them back as a slice of the id type.
pub unsafe trait TypedId: Copy + Eq + Ord + Hash + Debug {
    /// The id with this index; panics on `u32::MAX`.
    fn from_index(index: u32) -> Self;
    /// This id's index, counting from 0.
    fn index(self) -> u32;
}

/// Declares a typed id: `typed_id!(NodeId);`.
macro_rules! typed_id {
    ($(#[$attribute:meta])* $name:ident) => {
        $(#[$attribute])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[repr(transparent)]
        pub struct $name(std::num::NonZeroU32);

        impl $name {
            /// The id with this index. `u32::MAX` is given up so that `Option` fits in 4 bytes.
            pub const fn from_index(index: u32) -> $name {
                match std::num::NonZeroU32::new(index.wrapping_add(1)) {
                    Some(stored) => $name(stored),
                    None => panic!("Id index out of range."),
                }
            }

            pub const fn index(self) -> u32 {
                self.0.get() - 1
            }
        }

        // SAFETY: `repr(transparent)` over a `NonZeroU32` holding `index + 1`.
        unsafe impl $crate::ir::TypedId for $name {
            fn from_index(index: u32) -> $name {
                $name::from_index(index)
            }

            fn index(self) -> u32 {
                $name::index(self)
            }
        }

        impl std::fmt::Debug for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(formatter, "{}({})", stringify!($name), self.index())
            }
        }
    };
}
pub(crate) use typed_id;

fn position<Id: TypedId>(id: Id) -> usize {
    id.index() as usize
}

fn id_at<Id: TypedId>(position: usize) -> Id {
    Id::from_index(u32::try_from(position).expect("Too many ids."))
}

/// Owns its items and creates their ids: `push` returns the next id, and only that id type
/// indexes the vector.
#[derive(Debug, Clone)]
pub struct IdVec<Id, T> {
    items: Vec<T>,
    id: PhantomData<Id>,
}

impl<Id, T> Default for IdVec<Id, T> {
    fn default() -> Self {
        IdVec {
            items: Vec::new(),
            id: PhantomData,
        }
    }
}

impl<Id: TypedId, T> IdVec<Id, T> {
    pub fn with_capacity(capacity: usize) -> Self {
        IdVec {
            items: Vec::with_capacity(capacity),
            id: PhantomData,
        }
    }

    /// Stores `item` and returns its new id.
    pub fn push(&mut self, item: T) -> Id {
        let id = self.next_id();
        self.items.push(item);
        id
    }

    /// The id the next `push` will return.
    pub fn next_id(&self) -> Id {
        id_at(self.items.len())
    }

    pub fn get(&self, id: Id) -> Option<&T> {
        self.items.get(position(id))
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.items.capacity()
    }

    /// Forgets every item; ids start again from zero.
    pub fn clear(&mut self) {
        self.items.clear();
    }

    pub fn iter(&self) -> impl Iterator<Item = (Id, &T)> {
        self.items
            .iter()
            .enumerate()
            .map(|(index, item)| (id_at(index), item))
    }

    /// The items in id order, consuming the vector.
    pub fn into_items(self) -> impl Iterator<Item = T> {
        self.items.into_iter()
    }
}

impl<Id: TypedId, T> Index<Id> for IdVec<Id, T> {
    type Output = T;

    fn index(&self, id: Id) -> &T {
        &self.items[position(id)]
    }
}

impl<Id: TypedId, T> IndexMut<Id> for IdVec<Id, T> {
    fn index_mut(&mut self, id: Id) -> &mut T {
        &mut self.items[position(id)]
    }
}

/// Extra data about items another container owns, indexed by their ids. An entry never written
/// reads as the default, so the table grows only as far as the highest id actually written.
#[derive(Debug, Clone)]
pub struct SideTable<Id, T> {
    items: Vec<T>,
    default: T,
    id: PhantomData<Id>,
}

impl<Id, T: Default> Default for SideTable<Id, T> {
    fn default() -> Self {
        SideTable::new(T::default())
    }
}

impl<Id, T> SideTable<Id, T> {
    pub fn new(default: T) -> Self {
        SideTable {
            items: Vec::new(),
            default,
            id: PhantomData,
        }
    }

    /// Resets every entry to the default.
    pub fn clear(&mut self) {
        self.items.clear();
    }
}

impl<Id: TypedId, T> Index<Id> for SideTable<Id, T> {
    type Output = T;

    fn index(&self, id: Id) -> &T {
        self.items.get(position(id)).unwrap_or(&self.default)
    }
}

impl<Id: TypedId, T: Clone> IndexMut<Id> for SideTable<Id, T> {
    fn index_mut(&mut self, id: Id) -> &mut T {
        let position = position(id);
        if position >= self.items.len() {
            self.items.resize(position + 1, self.default.clone());
        }
        &mut self.items[position]
    }
}

/// A list of ids stored in a [`ListPool`]: 4 bytes, where a `Vec` would take 24. It is only an
/// index, so it means nothing without the pool it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct List<T> {
    /// Where the first item sits in the pool; 0 is the empty list, which takes no space.
    start: u32,
    item: PhantomData<T>,
}

impl<T> List<T> {
    pub const EMPTY: List<T> = List {
        start: 0,
        item: PhantomData,
    };

    pub fn is_empty(self) -> bool {
        self.start == 0
    }
}

impl<T> Default for List<T> {
    fn default() -> Self {
        List::EMPTY
    }
}

/// Storage for every [`List`] of one function, whatever their id type.
///
/// Each list lives in a block of 4, 8, 16, … slots: its length, then its items, then spare room,
/// so most insertions need no move. A list that outgrows its block moves to a block twice the
/// size, and the old block joins a free list for its size — linked through the free blocks
/// themselves — for the next list of that size to reuse. The design follows Cranelift's
/// `ListPool`.
#[derive(Debug, Clone, Default)]
pub struct ListPool {
    slots: Vec<u32>,
    /// Per size class, the most recently freed block plus one; 0 when none is free.
    free: Vec<u32>,
}

/// Size class `class` has blocks of `4 << class` slots.
fn block_size(class: usize) -> usize {
    4 << class
}

/// The smallest size class whose blocks hold `length` items and the length itself.
fn size_class(length: usize) -> usize {
    (length + 1).next_power_of_two().max(4).trailing_zeros() as usize - 2
}

impl ListPool {
    pub fn with_capacity(slots: usize) -> Self {
        ListPool {
            slots: Vec::with_capacity(slots),
            free: Vec::new(),
        }
    }

    /// Slots in use or on a free list; free blocks are never returned.
    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }

    /// Forgets every list; handles into the pool become meaningless.
    pub fn clear(&mut self) {
        self.slots.clear();
        self.free.clear();
    }

    /// A new list holding `items`.
    pub fn list<T: TypedId>(&mut self, items: &[T]) -> List<T> {
        if items.is_empty() {
            return List::EMPTY;
        }
        let block = self.allocate(size_class(items.len()));
        self.slots[block] = items.len() as u32;
        for (slot, item) in self.slots[block + 1..].iter_mut().zip(items) {
            *slot = item.index() + 1;
        }
        List {
            start: (block + 1) as u32,
            item: PhantomData,
        }
    }

    pub fn items<T: TypedId>(&self, list: List<T>) -> &[T] {
        let start = list.start as usize;
        let items = &self.slots[start..start + self.length(list)];
        // SAFETY: `T` is `repr(transparent)` over a `NonZeroU32` (the `TypedId` contract), and
        // item slots only ever hold `index + 1` of a `T`, which is never zero.
        unsafe { std::slice::from_raw_parts(items.as_ptr().cast::<T>(), items.len()) }
    }

    pub fn push<T: TypedId>(&mut self, list: &mut List<T>, item: T) {
        self.insert(list, self.length(*list), item);
    }

    /// Inserts `item` before position `at`; panics past the end, like `Vec::insert`.
    pub fn insert<T: TypedId>(&mut self, list: &mut List<T>, at: usize, item: T) {
        let length = self.length(*list);
        assert!(at <= length, "Insertion past the end of a list.");
        if list.is_empty() {
            *list = self.list(&[item]);
            return;
        }
        let (class, new_class) = (size_class(length), size_class(length + 1));
        if new_class != class {
            self.relocate(list, length, class, new_class);
        }
        let block = list.start as usize - 1;
        let items = block + 1;
        self.slots
            .copy_within(items + at..items + length, items + at + 1);
        self.slots[items + at] = item.index() + 1;
        self.slots[block] = (length + 1) as u32;
    }

    /// Removes and returns the item at position `at`; panics past the end, like `Vec::remove`.
    pub fn remove<T: TypedId>(&mut self, list: &mut List<T>, at: usize) -> T {
        let length = self.length(*list);
        assert!(at < length, "Removal past the end of a list.");
        let block = list.start as usize - 1;
        let items = block + 1;
        let removed = T::from_index(self.slots[items + at] - 1);
        self.slots
            .copy_within(items + at + 1..items + length, items + at);
        self.slots[block] = (length - 1) as u32;
        let (class, new_class) = (size_class(length), size_class(length - 1));
        if length == 1 {
            self.release(block, class);
            *list = List::EMPTY;
        } else if new_class != class {
            self.relocate(list, length - 1, class, new_class);
        }
        removed
    }

    /// Moves a list of `length` items from its block of size class `class` to a new block of
    /// `new_class`, so that a block's class always matches the length of the list in it.
    fn relocate<T>(&mut self, list: &mut List<T>, length: usize, class: usize, new_class: usize) {
        let block = list.start as usize - 1;
        let new_block = self.allocate(new_class);
        self.slots.copy_within(block..=block + length, new_block);
        self.release(block, class);
        list.start = (new_block + 1) as u32;
    }

    fn length<T>(&self, list: List<T>) -> usize {
        match list.start {
            0 => 0,
            start => self.slots[start as usize - 1] as usize,
        }
    }

    /// A free block of size class `class`: a reused one if any, otherwise new slots at the end.
    fn allocate(&mut self, class: usize) -> usize {
        match self.free.get(class) {
            Some(&head) if head != 0 => {
                let block = head as usize - 1;
                self.free[class] = self.slots[block + 1];
                block
            }
            _ => {
                let block = self.slots.len();
                self.slots.resize(block + block_size(class), 0);
                block
            }
        }
    }

    /// Puts a block on its size class's free list; its second slot links to the previous head.
    fn release(&mut self, block: usize, class: usize) {
        if self.free.len() <= class {
            self.free.resize(class + 1, 0);
        }
        self.slots[block] = 0;
        self.slots[block + 1] = self.free[class];
        self.free[class] = (block + 1) as u32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    typed_id!(AppleId);
    typed_id!(PearId);

    fn apple(index: u32) -> AppleId {
        AppleId::from_index(index)
    }

    fn apples(indices: impl IntoIterator<Item = u32>) -> Vec<AppleId> {
        indices.into_iter().map(apple).collect()
    }

    // -----------------------------------------------------------------------
    // Typed ids
    // -----------------------------------------------------------------------

    #[test]
    fn an_id_and_an_optional_id_are_four_bytes() {
        assert_eq!(size_of::<AppleId>(), 4);
        assert_eq!(size_of::<Option<AppleId>>(), 4);
    }

    #[test]
    fn an_id_round_trips_its_index() {
        for index in [0, 1, 7, 1 << 20, u32::MAX - 1] {
            assert_eq!(apple(index).index(), index);
        }
    }

    #[test]
    fn index_zero_is_a_real_id_not_none() {
        assert_ne!(Some(apple(0)), None);
    }

    #[test]
    #[should_panic]
    fn the_largest_index_is_given_up_for_the_niche() {
        apple(u32::MAX);
    }

    #[test]
    fn ids_order_by_index() {
        assert!(apple(1) < apple(2));
        assert!(apple(0) < apple(u32::MAX - 1));
    }

    // -----------------------------------------------------------------------
    // IdVec
    // -----------------------------------------------------------------------

    #[test]
    fn pushing_hands_out_consecutive_ids_from_zero() {
        let mut colours = IdVec::<AppleId, &str>::default();
        assert!(colours.is_empty());
        assert_eq!(colours.next_id(), apple(0));
        assert_eq!(colours.push("red"), apple(0));
        assert_eq!(colours.push("green"), apple(1));
        assert_eq!(colours.next_id(), apple(2));
        assert_eq!(colours.len(), 2);
    }

    #[test]
    fn an_item_reads_back_by_its_id_and_can_be_replaced_in_place() {
        let mut colours = IdVec::<AppleId, &str>::default();
        let red = colours.push("red");
        let green = colours.push("green");
        colours[red] = "yellow";
        assert_eq!(colours[red], "yellow");
        assert_eq!(colours[green], "green");
    }

    #[test]
    fn get_is_none_past_the_end() {
        let mut colours = IdVec::<AppleId, &str>::default();
        let red = colours.push("red");
        assert_eq!(colours.get(red), Some(&"red"));
        assert_eq!(colours.get(apple(1)), None);
    }

    #[test]
    #[should_panic]
    fn indexing_past_the_end_panics() {
        let colours = IdVec::<AppleId, &str>::default();
        let _ = colours[apple(0)];
    }

    #[test]
    fn iteration_yields_ids_with_their_items_in_order() {
        let mut colours = IdVec::<AppleId, &str>::default();
        let red = colours.push("red");
        let green = colours.push("green");
        let pairs: Vec<(AppleId, &&str)> = colours.iter().collect();
        assert_eq!(pairs, vec![(red, &"red"), (green, &"green")]);
    }

    #[test]
    fn consuming_yields_the_items_in_id_order() {
        let mut colours = IdVec::<AppleId, &str>::default();
        colours.push("red");
        colours.push("green");
        assert_eq!(colours.into_items().collect::<Vec<_>>(), ["red", "green"]);
    }

    #[test]
    fn clear_restarts_ids_from_zero() {
        let mut colours = IdVec::<AppleId, &str>::default();
        colours.push("red");
        colours.push("green");
        colours.clear();
        assert!(colours.is_empty());
        assert_eq!(colours.push("blue"), apple(0));
    }

    #[test]
    fn a_presized_vec_starts_empty_with_room_reserved() {
        let colours = IdVec::<AppleId, &str>::with_capacity(100);
        assert!(colours.is_empty());
        assert_eq!(colours.next_id(), apple(0));
        assert!(colours.capacity() >= 100);
    }

    // -----------------------------------------------------------------------
    // SideTable
    // -----------------------------------------------------------------------

    #[test]
    fn an_unwritten_entry_reads_as_the_default() {
        let weights = SideTable::<AppleId, u8>::new(7);
        assert_eq!(weights[apple(0)], 7);
        assert_eq!(weights[apple(3)], 7);
    }

    #[test]
    fn writing_past_the_end_fills_the_gap_with_the_default() {
        let mut weights = SideTable::<AppleId, u8>::new(7);
        weights[apple(5)] = 1;
        assert_eq!(weights[apple(4)], 7);
        assert_eq!(weights[apple(5)], 1);
        assert_eq!(weights[apple(6)], 7);
    }

    #[test]
    fn reading_far_past_the_end_does_not_panic() {
        let weights = SideTable::<AppleId, u8>::new(7);
        assert_eq!(weights[apple(u32::MAX - 1)], 7);
    }

    #[test]
    fn an_entry_can_be_overwritten() {
        let mut weights = SideTable::<AppleId, u8>::new(7);
        weights[apple(2)] = 1;
        weights[apple(2)] = 2;
        assert_eq!(weights[apple(2)], 2);
    }

    #[test]
    fn clear_resets_every_entry_to_the_default() {
        let mut weights = SideTable::<AppleId, u8>::new(7);
        weights[apple(0)] = 1;
        weights[apple(9)] = 1;
        weights.clear();
        assert_eq!(weights[apple(0)], 7);
        assert_eq!(weights[apple(9)], 7);
    }

    #[test]
    fn a_default_side_table_uses_the_type_default() {
        let partners = SideTable::<AppleId, Option<PearId>>::default();
        assert_eq!(partners[apple(4)], None);
    }

    // -----------------------------------------------------------------------
    // List and ListPool
    // -----------------------------------------------------------------------

    #[test]
    fn a_list_handle_is_four_bytes() {
        assert_eq!(size_of::<List<AppleId>>(), 4);
    }

    #[test]
    fn the_empty_list_takes_no_pool_space() {
        let mut pool = ListPool::default();
        let empty = pool.list::<AppleId>(&[]);
        assert!(empty.is_empty());
        assert!(pool.items(empty).is_empty());
        assert!(pool.items(List::<AppleId>::EMPTY).is_empty());
        assert_eq!(pool.slot_count(), 0);
    }

    #[test]
    fn a_list_reads_back_in_order_at_every_block_boundary() {
        // Lengths on both sides of each power-of-two block size (the length takes one slot).
        for length in [1, 2, 3, 4, 7, 8, 15, 16, 31, 32, 100] {
            let mut pool = ListPool::default();
            let items = apples((0..length).rev());
            let list = pool.list(&items);
            assert_eq!(pool.items(list), items.as_slice(), "length {length}");
        }
    }

    #[test]
    fn id_zero_survives_as_a_list_item() {
        // A freed block marks itself with 0; a live list full of id 0 must never read as freed.
        let mut pool = ListPool::default();
        let mut zeros = pool.list(&apples([0, 0, 0]));
        pool.push(&mut zeros, apple(0));
        let reused = pool.list(&apples([0, 0]));
        assert_eq!(pool.items(zeros), apples([0, 0, 0, 0]).as_slice());
        assert_eq!(pool.items(reused), apples([0, 0]).as_slice());
    }

    #[test]
    fn lists_in_one_pool_do_not_overlap() {
        let mut pool = ListPool::default();
        let contents: Vec<Vec<AppleId>> = (1..=12)
            .map(|length| apples((0..length).map(|index| length * 100 + index)))
            .collect();
        let lists: Vec<List<AppleId>> = contents.iter().map(|items| pool.list(items)).collect();
        for (list, items) in lists.iter().zip(&contents) {
            assert_eq!(pool.items(*list), items.as_slice());
        }
    }

    #[test]
    fn pushing_grows_a_list_without_disturbing_its_neighbours() {
        let mut pool = ListPool::default();
        let before = pool.list(&apples([90, 91]));
        let mut growing = pool.list(&apples([0, 1, 2]));
        let after = pool.list(&apples([95, 96, 97]));
        for index in 3..40 {
            pool.push(&mut growing, apple(index));
        }
        assert_eq!(pool.items(growing), apples(0..40).as_slice());
        assert_eq!(pool.items(before), apples([90, 91]).as_slice());
        assert_eq!(pool.items(after), apples([95, 96, 97]).as_slice());
    }

    #[test]
    fn pushing_onto_the_empty_list_starts_a_list() {
        let mut pool = ListPool::default();
        let mut list = List::EMPTY;
        pool.push(&mut list, apple(5));
        assert!(!list.is_empty());
        assert_eq!(pool.items(list), &[apple(5)]);
    }

    #[test]
    fn insert_places_an_item_at_the_front_middle_and_end() {
        let mut pool = ListPool::default();
        let mut list = pool.list(&apples([1, 3]));
        pool.insert(&mut list, 0, apple(0));
        pool.insert(&mut list, 2, apple(2));
        pool.insert(&mut list, 4, apple(4));
        assert_eq!(pool.items(list), apples(0..5).as_slice());
    }

    #[test]
    #[should_panic]
    fn inserting_past_the_end_panics() {
        let mut pool = ListPool::default();
        let mut list = pool.list(&apples([1]));
        pool.insert(&mut list, 2, apple(2));
    }

    #[test]
    fn remove_returns_the_item_and_closes_the_gap() {
        let mut pool = ListPool::default();
        let mut list = pool.list(&apples(0..5));
        assert_eq!(pool.remove(&mut list, 2), apple(2));
        assert_eq!(pool.remove(&mut list, 0), apple(0));
        assert_eq!(pool.remove(&mut list, 2), apple(4));
        assert_eq!(pool.items(list), apples([1, 3]).as_slice());
    }

    #[test]
    fn removing_the_last_item_leaves_the_empty_list() {
        let mut pool = ListPool::default();
        let mut list = pool.list(&apples([7]));
        pool.remove(&mut list, 0);
        assert!(list.is_empty());
        assert!(pool.items(list).is_empty());
    }

    #[test]
    #[should_panic]
    fn removing_from_the_empty_list_panics() {
        let mut pool = ListPool::default();
        let mut list = List::<AppleId>::EMPTY;
        pool.remove(&mut list, 0);
    }

    #[test]
    fn a_block_given_up_by_growth_is_reused_by_the_next_list_of_its_size() {
        let mut pool = ListPool::default();
        let mut growing = pool.list(&apples([0, 1, 2]));
        pool.push(&mut growing, apple(3));
        let slots_after_growth = pool.slot_count();
        let reusing = pool.list(&apples([7, 8, 9]));
        assert_eq!(pool.slot_count(), slots_after_growth);
        assert_eq!(pool.items(reusing), apples([7, 8, 9]).as_slice());
        assert_eq!(pool.items(growing), apples(0..4).as_slice());
    }

    #[test]
    fn a_block_given_up_by_emptying_is_reused() {
        let mut pool = ListPool::default();
        let mut emptied = pool.list(&apples([0, 1]));
        pool.remove(&mut emptied, 0);
        pool.remove(&mut emptied, 0);
        let slots = pool.slot_count();
        let reusing = pool.list(&apples([5, 6]));
        assert_eq!(pool.slot_count(), slots);
        assert_eq!(pool.items(reusing), apples([5, 6]).as_slice());
    }

    #[test]
    fn one_pool_holds_lists_of_different_id_types() {
        let mut pool = ListPool::default();
        let apple_list = pool.list(&apples([1, 2, 3]));
        let pear_list = pool.list(&[PearId::from_index(9), PearId::from_index(8)]);
        assert_eq!(pool.items(apple_list), apples([1, 2, 3]).as_slice());
        assert_eq!(
            pool.items(pear_list),
            &[PearId::from_index(9), PearId::from_index(8)]
        );
    }

    #[test]
    fn clear_empties_the_pool() {
        let mut pool = ListPool::default();
        pool.list(&apples(0..10));
        pool.clear();
        assert_eq!(pool.slot_count(), 0);
        let fresh = pool.list(&apples([4]));
        assert_eq!(pool.items(fresh), &[apple(4)]);
    }

    #[test]
    fn a_presized_pool_starts_empty() {
        let pool = ListPool::with_capacity(64);
        assert_eq!(pool.slot_count(), 0);
    }

    /// Hundreds of interleaved pushes, inserts and removes over several lists, checked after every
    /// step against plain `Vec`s. A deterministic generator keeps failures reproducible.
    #[test]
    fn random_edits_match_a_vec_model() {
        let mut seed: u64 = 0x5eed;
        let mut next = move |bound: usize| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as usize % bound
        };
        let mut pool = ListPool::default();
        let mut lists = [List::<AppleId>::EMPTY; 5];
        let mut models: Vec<Vec<AppleId>> = vec![Vec::new(); 5];
        for step in 0..2000u32 {
            let which = next(lists.len());
            let (list, model) = (&mut lists[which], &mut models[which]);
            match next(3) {
                0 => {
                    pool.push(list, apple(step));
                    model.push(apple(step));
                }
                1 => {
                    let position = next(model.len() + 1);
                    pool.insert(list, position, apple(step));
                    model.insert(position, apple(step));
                }
                _ if !model.is_empty() => {
                    let position = next(model.len());
                    assert_eq!(pool.remove(list, position), model.remove(position));
                }
                _ => {}
            }
            for (list, model) in lists.iter().zip(&models) {
                assert_eq!(pool.items(*list), model.as_slice(), "after step {step}");
            }
        }
    }
}
