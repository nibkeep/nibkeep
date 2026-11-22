//! Small sets and maps: they behave as `BTreeSet` and `BTreeMap` do, whatever their size.

use std::collections::{BTreeMap, BTreeSet};

use nibkeep_base::small::{SmallMap, SmallSet};
use proptest::prelude::*;

/// An operation on a set, or on a map with the value given.
#[derive(Clone, Debug)]
enum Step {
	Insert(u8, u16),
	Remove(u8),
}

/// Keys from a range a little larger than the size at which sets change form, so that
/// inserts and removals meet.
fn key() -> impl Strategy<Value = u8> {
	0..40_u8
}

/// Steps that grow a set past the size at which it becomes a tree, change it there, and
/// shrink it back into a vector: a run of inserts, a mix, and a run of removals.
fn steps() -> impl Strategy<Value = Vec<Step>> {
	let insert = || (key(), any::<u16>()).prop_map(|(key, value)| Step::Insert(key, value));
	let remove = || key().prop_map(Step::Remove);
	(
		prop::collection::vec(insert(), 0..60),
		prop::collection::vec(prop_oneof![insert(), remove()], 0..200),
		prop::collection::vec(remove(), 0..80),
	)
		.prop_map(|(grow, mix, shrink)| [grow, mix, shrink].concat())
}

proptest! {
	#[test]
	fn a_small_set_is_a_btree_set(steps in steps(), probes in prop::collection::vec(key(), 8)) {
		let mut small = SmallSet::new();
		let mut known = BTreeSet::new();
		for step in steps {
			match step {
				Step::Insert(key, _) => prop_assert_eq!(small.insert(key), known.insert(key)),
				Step::Remove(key) => prop_assert_eq!(small.remove(&key), known.remove(&key)),
			}
			prop_assert_eq!(small.len(), known.len());
			prop_assert_eq!(small.is_empty(), known.is_empty());
			prop_assert!(small.iter().eq(known.iter()), "the same elements, in order");
			prop_assert_eq!(small.first(), known.first());
			for probe in &probes {
				prop_assert_eq!(small.contains(probe), known.contains(probe));
			}
		}
		let collected: SmallSet<u8> = known.iter().copied().collect();
		prop_assert_eq!(&collected, &small, "equal whatever the way they were made");
		prop_assert!((&small).into_iter().eq(known.iter()));
	}

	#[test]
	fn a_small_map_is_a_btree_map(steps in steps(), probes in prop::collection::vec(key(), 8)) {
		let mut small = SmallMap::new();
		let mut known = BTreeMap::new();
		for step in steps {
			match step {
				Step::Insert(key, value) => {
					prop_assert_eq!(small.insert(key, value), known.insert(key, value));
				}
				Step::Remove(key) => prop_assert_eq!(small.remove(&key), known.remove(&key)),
			}
			prop_assert_eq!(small.len(), known.len());
			prop_assert_eq!(small.is_empty(), known.is_empty());
			prop_assert!(small.iter().eq(known.iter()), "the same entries, in order");
			prop_assert!(small.keys().eq(known.keys()));
			prop_assert!(small.values().eq(known.values()));
			for probe in &probes {
				prop_assert_eq!(small.get(probe), known.get(probe));
			}
		}
		// Changing values in place, and adding through the entry of a missing key.
		for probe in probes {
			let value = small.get_or_insert_with(probe, || 7);
			*value = value.wrapping_add(1);
			let value = known.entry(probe).or_insert(7);
			*value = value.wrapping_add(1);
			if let Some(value) = small.get_mut(&probe.wrapping_add(1)) {
				*value = 0;
			}
			if let Some(value) = known.get_mut(&probe.wrapping_add(1)) {
				*value = 0;
			}
		}
		prop_assert!(small.iter().eq(known.iter()));
	}
}

#[test]
fn a_set_grows_into_a_tree_and_shrinks_back() {
	let mut set: SmallSet<u8> = (0..40).collect();
	for key in 0..36 {
		assert!(set.remove(&key));
	}
	assert_eq!(set, SmallSet::from([36, 37, 38, 39]));
	assert!(set.insert(0));
	assert_eq!(set.first(), Some(&0));
	assert_eq!(set.iter().len(), 5);
}
