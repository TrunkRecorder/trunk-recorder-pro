//! A fixed-capacity buffer that overwrites its oldest item.

use std::collections::VecDeque;

#[derive(Clone, Debug)]
pub struct Ring<T> {
    items: VecDeque<T>,
    cap: usize,
}

impl<T> Ring<T> {
    pub fn new(cap: usize) -> Self {
        Ring { items: VecDeque::new(), cap: cap.max(1) }
    }

    pub fn push(&mut self, v: T) {
        if self.items.len() == self.cap {
            self.items.pop_front();
        }
        self.items.push_back(v);
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Oldest first.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &T> + ExactSizeIterator {
        self.items.iter()
    }

    pub fn last(&self) -> Option<&T> {
        self.items.back()
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::Ring;

    #[test]
    fn keeps_the_newest() {
        let mut r = Ring::new(3);
        for i in 0..5 {
            r.push(i);
        }
        assert_eq!(r.iter().copied().collect::<Vec<_>>(), vec![2, 3, 4]);
        assert_eq!(r.last(), Some(&4));
    }
}
