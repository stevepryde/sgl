//! Retention of the current and previous pipeline specialisation: repeated
//! use of one value does not age the previous one, and returning to it is a
//! cache hit. Each stage retains every subvariant of those two values.
use std::collections::HashMap;
use std::hash::Hash;

#[derive(Default)]
pub(crate) struct PipelineSpecialisations<T> {
    current: Option<T>,
    previous: Option<T>,
}

impl<T: Copy + Eq> PipelineSpecialisations<T> {
    pub fn retain<K: Eq + Hash, P>(
        &mut self,
        pipelines: &mut HashMap<K, P>,
        current: T,
        specialisation: impl Fn(&K) -> T,
    ) {
        if self.current == Some(current) {
            return;
        }
        self.previous = self.current.replace(current);
        pipelines.retain(|key, _| {
            let value = specialisation(key);
            value == current || Some(value) == self.previous
        });
    }
}
