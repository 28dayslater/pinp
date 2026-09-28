// SPDX-License-Identifier: MIT

//! Typed ids and the containers the IR is built from: `IdVec` (owns items and creates their ids),
//! `SideTable` (per-id data with a default), and `List` handles into a shared `ListPool`.

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
        let mut lists = vec![List::<AppleId>::EMPTY; 5];
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
