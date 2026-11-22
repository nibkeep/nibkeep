//! Sets and maps that are small in memory while they are small in size.
//!
//! A `BTreeSet` or `BTreeMap` with one element allocates a whole node, with room for
//! eleven, which for an ID of 16 bytes and its value comes to hundreds of bytes. Most of
//! the sets that index a library hold one element or a few: the statements of one field of
//! an entity, or the parents of a change. These keep up to [`FEW`] elements in a sorted
//! vector of exactly their size, and turn into a B-tree beyond that, so that a set that
//! grows large, such as the statements that hold a common word, stays quick to change.

use std::collections::{BTreeMap, BTreeSet, btree_map, btree_set};
use std::fmt;
use std::{iter, mem, slice};

/// The most elements kept in a sorted vector. A set or map of more is a B-tree, until it
/// shrinks to half as many.
pub const FEW: usize = 16;

/// Gives back a vector's spare room once it is half empty: often enough that a set that
/// shrank costs little, and seldom enough that removing and adding again does not move it
/// each time. Adding grows a vector by one element only, as most sets stay small.
fn shrink<T>(vector: &mut Vec<T>) {
	if vector.capacity() >= 2 * vector.len().max(1) {
		vector.shrink_to_fit();
	}
}

/// A set, ordered, small in memory while it holds few elements.
#[derive(Clone)]
pub struct SmallSet<T>(SetRepr<T>);

#[derive(Clone)]
enum SetRepr<T> {
	/// Sorted, without duplicates, with no spare capacity.
	Few(Vec<T>),
	Many(BTreeSet<T>),
}

impl<T> SmallSet<T> {
	/// An empty set, which allocates nothing.
	#[must_use]
	pub const fn new() -> Self {
		Self(SetRepr::Few(Vec::new()))
	}

	/// How many elements it holds.
	#[must_use]
	pub fn len(&self) -> usize {
		match &self.0 {
			SetRepr::Few(few) => few.len(),
			SetRepr::Many(many) => many.len(),
		}
	}

	/// Whether it holds none.
	#[must_use]
	pub fn is_empty(&self) -> bool {
		self.len() == 0
	}

	/// The elements, in order.
	#[must_use]
	pub fn iter(&self) -> SetIter<'_, T> {
		match &self.0 {
			SetRepr::Few(few) => SetIter(SetIterRepr::Few(few.iter())),
			SetRepr::Many(many) => SetIter(SetIterRepr::Many(many.iter())),
		}
	}

	/// The smallest element.
	#[must_use]
	pub fn first(&self) -> Option<&T>
	where
		T: Ord,
	{
		match &self.0 {
			SetRepr::Few(few) => few.first(),
			SetRepr::Many(many) => many.first(),
		}
	}
}

impl<T: Ord> SmallSet<T> {
	/// Whether it holds an element.
	#[must_use]
	pub fn contains(&self, value: &T) -> bool {
		match &self.0 {
			SetRepr::Few(few) => few.binary_search(value).is_ok(),
			SetRepr::Many(many) => many.contains(value),
		}
	}

	/// Adds an element, and returns whether it was new.
	pub fn insert(&mut self, value: T) -> bool {
		match &mut self.0 {
			SetRepr::Few(few) => match few.binary_search(&value) {
				Ok(_) => false,
				Err(_) if few.len() == FEW => {
					let mut many: BTreeSet<T> = mem::take(few).into_iter().collect();
					many.insert(value);
					self.0 = SetRepr::Many(many);
					true
				}
				Err(index) => {
					few.reserve_exact(1);
					few.insert(index, value);
					true
				}
			},
			SetRepr::Many(many) => many.insert(value),
		}
	}

	/// Removes an element, and returns whether it was there.
	pub fn remove(&mut self, value: &T) -> bool {
		match &mut self.0 {
			SetRepr::Few(few) => match few.binary_search(value) {
				Ok(index) => {
					few.remove(index);
					shrink(few);
					true
				}
				Err(_) => false,
			},
			SetRepr::Many(many) => {
				let removed = many.remove(value);
				if many.len() <= FEW / 2 {
					let few: Vec<T> = mem::take(many).into_iter().collect();
					self.0 = SetRepr::Few(few);
				}
				removed
			}
		}
	}
}

impl<T> Default for SmallSet<T> {
	fn default() -> Self {
		Self::new()
	}
}

impl<T: fmt::Debug> fmt::Debug for SmallSet<T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_set().entries(self.iter()).finish()
	}
}

impl<T: PartialEq> PartialEq for SmallSet<T> {
	fn eq(&self, other: &Self) -> bool {
		self.len() == other.len() && self.iter().eq(other.iter())
	}
}

impl<T: Eq> Eq for SmallSet<T> {}

impl<T: Ord> FromIterator<T> for SmallSet<T> {
	fn from_iter<I: IntoIterator<Item = T>>(elements: I) -> Self {
		let mut set = Self::new();
		set.extend(elements);
		set
	}
}

impl<T: Ord, const N: usize> From<[T; N]> for SmallSet<T> {
	fn from(elements: [T; N]) -> Self {
		elements.into_iter().collect()
	}
}

impl<T: Ord> Extend<T> for SmallSet<T> {
	fn extend<I: IntoIterator<Item = T>>(&mut self, elements: I) {
		for element in elements {
			self.insert(element);
		}
	}
}

impl<'a, T> IntoIterator for &'a SmallSet<T> {
	type Item = &'a T;
	type IntoIter = SetIter<'a, T>;

	fn into_iter(self) -> Self::IntoIter {
		self.iter()
	}
}

/// The elements of a [`SmallSet`], in order.
#[derive(Clone, Debug)]
pub struct SetIter<'a, T>(SetIterRepr<'a, T>);

#[derive(Clone, Debug)]
enum SetIterRepr<'a, T> {
	Few(slice::Iter<'a, T>),
	Many(btree_set::Iter<'a, T>),
}

impl<'a, T> Iterator for SetIter<'a, T> {
	type Item = &'a T;

	fn next(&mut self) -> Option<Self::Item> {
		match &mut self.0 {
			SetIterRepr::Few(few) => few.next(),
			SetIterRepr::Many(many) => many.next(),
		}
	}

	fn size_hint(&self) -> (usize, Option<usize>) {
		match &self.0 {
			SetIterRepr::Few(few) => few.size_hint(),
			SetIterRepr::Many(many) => many.size_hint(),
		}
	}
}

impl<T> DoubleEndedIterator for SetIter<'_, T> {
	fn next_back(&mut self) -> Option<Self::Item> {
		match &mut self.0 {
			SetIterRepr::Few(few) => few.next_back(),
			SetIterRepr::Many(many) => many.next_back(),
		}
	}
}

impl<T> ExactSizeIterator for SetIter<'_, T> {}

impl<T> iter::FusedIterator for SetIter<'_, T> {}

/// A map, ordered by key, small in memory while it holds few entries.
#[derive(Clone)]
pub struct SmallMap<K, V>(MapRepr<K, V>);

#[derive(Clone)]
enum MapRepr<K, V> {
	/// Sorted by key, without duplicate keys, with no spare capacity.
	Few(Vec<(K, V)>),
	Many(BTreeMap<K, V>),
}

impl<K, V> SmallMap<K, V> {
	/// An empty map, which allocates nothing.
	#[must_use]
	pub const fn new() -> Self {
		Self(MapRepr::Few(Vec::new()))
	}

	/// How many entries it holds.
	#[must_use]
	pub fn len(&self) -> usize {
		match &self.0 {
			MapRepr::Few(few) => few.len(),
			MapRepr::Many(many) => many.len(),
		}
	}

	/// Whether it holds none.
	#[must_use]
	pub fn is_empty(&self) -> bool {
		self.len() == 0
	}

	/// The entries, in the order of their keys.
	#[must_use]
	pub fn iter(&self) -> MapIter<'_, K, V> {
		match &self.0 {
			MapRepr::Few(few) => MapIter(MapIterRepr::Few(few.iter())),
			MapRepr::Many(many) => MapIter(MapIterRepr::Many(many.iter())),
		}
	}

	/// The keys, in order.
	pub fn keys(&self) -> impl DoubleEndedIterator<Item = &K> + ExactSizeIterator {
		self.iter().map(|(key, _)| key)
	}

	/// The values, in the order of their keys.
	pub fn values(&self) -> impl DoubleEndedIterator<Item = &V> + ExactSizeIterator {
		self.iter().map(|(_, value)| value)
	}
}

impl<K: Ord, V> SmallMap<K, V> {
	/// The value of a key.
	#[must_use]
	pub fn get(&self, key: &K) -> Option<&V> {
		match &self.0 {
			MapRepr::Few(few) => few
				.binary_search_by(|(held, _)| held.cmp(key))
				.ok()
				.map(|index| &few[index].1),
			MapRepr::Many(many) => many.get(key),
		}
	}

	/// The value of a key, to change.
	#[must_use]
	pub fn get_mut(&mut self, key: &K) -> Option<&mut V> {
		match &mut self.0 {
			MapRepr::Few(few) => few
				.binary_search_by(|(held, _)| held.cmp(key))
				.ok()
				.map(|index| &mut few[index].1),
			MapRepr::Many(many) => many.get_mut(key),
		}
	}

	/// Whether it holds a key.
	#[must_use]
	pub fn contains_key(&self, key: &K) -> bool {
		self.get(key).is_some()
	}

	/// Sets the value of a key, and returns the value it had.
	pub fn insert(&mut self, key: K, value: V) -> Option<V> {
		match &mut self.0 {
			MapRepr::Few(few) => match few.binary_search_by(|(held, _)| held.cmp(&key)) {
				Ok(index) => Some(mem::replace(&mut few[index].1, value)),
				Err(_) if few.len() == FEW => {
					let mut many: BTreeMap<K, V> = mem::take(few).into_iter().collect();
					many.insert(key, value);
					self.0 = MapRepr::Many(many);
					None
				}
				Err(index) => {
					few.reserve_exact(1);
					few.insert(index, (key, value));
					None
				}
			},
			MapRepr::Many(many) => many.insert(key, value),
		}
	}

	/// The value of a key, which `default` makes if the map has none, to change.
	pub fn get_or_insert_with(&mut self, key: K, default: impl FnOnce() -> V) -> &mut V {
		// Where the key is or goes in a vector; none for a tree, or for a new key in a full
		// vector, which turns it into one.
		let position = match &self.0 {
			MapRepr::Few(few) => {
				let position = few.binary_search_by(|(held, _)| held.cmp(&key));
				(position.is_ok() || few.len() < FEW).then_some(position)
			}
			MapRepr::Many(_) => None,
		};
		if position.is_none()
			&& let MapRepr::Few(few) = &mut self.0
		{
			self.0 = MapRepr::Many(mem::take(few).into_iter().collect());
		}
		match (&mut self.0, position) {
			(MapRepr::Few(few), Some(Ok(index))) => &mut few[index].1,
			(MapRepr::Few(few), Some(Err(index))) => {
				few.reserve_exact(1);
				few.insert(index, (key, default()));
				&mut few[index].1
			}
			(MapRepr::Many(many), _) => many.entry(key).or_insert_with(default),
			(MapRepr::Few(_), None) => unreachable!("a full vector became a tree"),
		}
	}

	/// Removes a key, and returns its value.
	pub fn remove(&mut self, key: &K) -> Option<V> {
		match &mut self.0 {
			MapRepr::Few(few) => {
				let index = few.binary_search_by(|(held, _)| held.cmp(key)).ok()?;
				let (_, value) = few.remove(index);
				shrink(few);
				Some(value)
			}
			MapRepr::Many(many) => {
				let removed = many.remove(key);
				if many.len() <= FEW / 2 {
					let few: Vec<(K, V)> = mem::take(many).into_iter().collect();
					self.0 = MapRepr::Few(few);
				}
				removed
			}
		}
	}
}

impl<K, V> Default for SmallMap<K, V> {
	fn default() -> Self {
		Self::new()
	}
}

impl<K: fmt::Debug, V: fmt::Debug> fmt::Debug for SmallMap<K, V> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_map().entries(self.iter()).finish()
	}
}

impl<K: PartialEq, V: PartialEq> PartialEq for SmallMap<K, V> {
	fn eq(&self, other: &Self) -> bool {
		self.len() == other.len() && self.iter().eq(other.iter())
	}
}

impl<K: Eq, V: Eq> Eq for SmallMap<K, V> {}

impl<'a, K, V> IntoIterator for &'a SmallMap<K, V> {
	type Item = (&'a K, &'a V);
	type IntoIter = MapIter<'a, K, V>;

	fn into_iter(self) -> Self::IntoIter {
		self.iter()
	}
}

/// The entries of a [`SmallMap`], in the order of their keys.
#[derive(Clone, Debug)]
pub struct MapIter<'a, K, V>(MapIterRepr<'a, K, V>);

#[derive(Clone, Debug)]
enum MapIterRepr<'a, K, V> {
	Few(slice::Iter<'a, (K, V)>),
	Many(btree_map::Iter<'a, K, V>),
}

impl<'a, K, V> Iterator for MapIter<'a, K, V> {
	type Item = (&'a K, &'a V);

	fn next(&mut self) -> Option<Self::Item> {
		match &mut self.0 {
			MapIterRepr::Few(few) => few.next().map(|(key, value)| (key, value)),
			MapIterRepr::Many(many) => many.next(),
		}
	}

	fn size_hint(&self) -> (usize, Option<usize>) {
		match &self.0 {
			MapIterRepr::Few(few) => few.size_hint(),
			MapIterRepr::Many(many) => many.size_hint(),
		}
	}
}

impl<K, V> DoubleEndedIterator for MapIter<'_, K, V> {
	fn next_back(&mut self) -> Option<Self::Item> {
		match &mut self.0 {
			MapIterRepr::Few(few) => few.next_back().map(|(key, value)| (key, value)),
			MapIterRepr::Many(many) => many.next_back(),
		}
	}
}

impl<K, V> ExactSizeIterator for MapIter<'_, K, V> {}

impl<K, V> iter::FusedIterator for MapIter<'_, K, V> {}
