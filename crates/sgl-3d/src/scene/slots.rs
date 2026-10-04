//! Content stored at the indices of the identities a scene issued. Fresh
//! indices allocate in order from 0; a removed index is reused, lowest
//! first, under a new generation, so an identity of removed content, or one
//! another scene issued, finds nothing.
use crate::content::identity::Identity;
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::marker::PhantomData;

pub(crate) struct Slots<I, T> {
    entries: Vec<Option<(u64, T)>>,
    free: BinaryHeap<Reverse<usize>>,
    identity: PhantomData<I>,
}

impl<I: Identity, T> Default for Slots<I, T> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            free: BinaryHeap::new(),
            identity: PhantomData,
        }
    }
}

impl<I: Identity, T> Slots<I, T> {
    /// The index the next insertion takes.
    pub fn next_index(&self) -> usize {
        self.free
            .peek()
            .map_or(self.entries.len(), |Reverse(index)| *index)
    }

    /// Stores `value` at `next_index` under a generation no identity had.
    pub fn insert(&mut self, value: T) -> I {
        let index = match self.free.pop() {
            Some(Reverse(index)) => index,
            None => {
                self.entries.push(None);
                self.entries.len() - 1
            }
        };
        let generation = super::next_generation();
        self.entries[index] = Some((generation, value));
        I::issue(index, generation)
    }

    pub fn get(&self, id: I) -> Option<&T> {
        match self.entries.get(id.index()) {
            Some(Some((generation, value))) if *generation == id.generation() => Some(value),
            _ => None,
        }
    }

    pub fn get_mut(&mut self, id: I) -> Option<&mut T> {
        match self.entries.get_mut(id.index()) {
            Some(Some((generation, value))) if *generation == id.generation() => Some(value),
            _ => None,
        }
    }

    /// Ends `id`, freeing its index.
    pub fn remove(&mut self, id: I) -> Option<T> {
        self.get(id)?;
        let (_, value) = self.entries[id.index()].take()?;
        self.free.push(Reverse(id.index()));
        Some(value)
    }

    /// Whether no content is live.
    pub fn is_empty(&self) -> bool {
        self.free.len() == self.entries.len()
    }

    /// The content at `index`, whatever its generation.
    pub fn at(&self, index: usize) -> Option<&T> {
        self.entries.get(index)?.as_ref().map(|(_, value)| value)
    }

    /// Live content in index order.
    pub fn iter(&self) -> impl Iterator<Item = (I, &T)> {
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                entry
                    .as_ref()
                    .map(|(generation, value)| (I::issue(index, *generation), value))
            })
    }

    /// Live content in index order.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (I, &mut T)> {
        self.entries
            .iter_mut()
            .enumerate()
            .filter_map(|(index, entry)| {
                entry
                    .as_mut()
                    .map(|(generation, value)| (I::issue(index, *generation), value))
            })
    }
}
