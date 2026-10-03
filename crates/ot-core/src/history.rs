//! Fixed-capacity ring buffer for time series.
//!
//! Every graph in the app is a window over one of these. Capacity is fixed at
//! construction so a long-running session has a hard memory ceiling: no
//! ever-growing `Vec`, no periodic compaction pauses.

use std::collections::VecDeque;

/// A bounded FIFO that drops its oldest element when full.
#[derive(Debug, Clone)]
pub struct Ring<T> {
    buf: VecDeque<T>,
    cap: usize,
}

impl<T> Ring<T> {
    /// Create a ring holding at most `cap` elements. `cap` of zero is clamped to one.
    #[must_use]
    pub fn new(cap: usize) -> Self {
        let cap = cap.max(1);
        Self {
            buf: VecDeque::with_capacity(cap),
            cap,
        }
    }

    /// Append, evicting the oldest element if at capacity.
    pub fn push(&mut self, value: T) {
        if self.buf.len() == self.cap {
            self.buf.pop_front();
        }
        self.buf.push_back(value);
    }

    #[must_use]
    pub fn capacity(&self) -> usize {
        self.cap
    }

    /// Change the capacity, keeping the newest elements that still fit. `cap` of
    /// zero is clamped to one.
    pub fn set_capacity(&mut self, cap: usize) {
        let cap = cap.max(1);
        while self.buf.len() > cap {
            self.buf.pop_front();
        }
        if cap > self.cap {
            self.buf.reserve_exact(cap - self.buf.len());
        } else {
            self.buf.shrink_to(cap);
        }
        self.cap = cap;
    }

    /// Oldest element still held.
    #[must_use]
    pub fn oldest(&self) -> Option<&T> {
        self.buf.front()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Most recently pushed element.
    #[must_use]
    pub fn latest(&self) -> Option<&T> {
        self.buf.back()
    }

    /// Oldest to newest.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &T> + ExactSizeIterator {
        self.buf.iter()
    }

    /// Discard all elements, keeping capacity.
    pub fn clear(&mut self) {
        self.buf.clear();
    }
}

impl<'a, T> IntoIterator for &'a Ring<T> {
    type Item = &'a T;
    type IntoIter = std::collections::vec_deque::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.buf.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::Ring;

    #[test]
    fn evicts_oldest_when_full() {
        let mut r = Ring::new(3);
        for i in 0..5 {
            r.push(i);
        }
        assert_eq!(r.len(), 3);
        assert_eq!(r.iter().copied().collect::<Vec<_>>(), vec![2, 3, 4]);
        assert_eq!(r.latest(), Some(&4));
    }

    #[test]
    fn resizing_keeps_the_newest() {
        let mut r = Ring::new(5);
        for i in 0..5 {
            r.push(i);
        }
        r.set_capacity(2);
        assert_eq!(r.iter().copied().collect::<Vec<_>>(), vec![3, 4]);
        assert_eq!(r.oldest(), Some(&3));
        r.set_capacity(4);
        r.push(5);
        r.push(6);
        r.push(7);
        assert_eq!(r.iter().copied().collect::<Vec<_>>(), vec![4, 5, 6, 7]);
        assert_eq!(r.capacity(), 4);
    }

    #[test]
    fn zero_capacity_is_clamped_to_one() {
        let mut r = Ring::new(0);
        r.push('a');
        r.push('b');
        assert_eq!(r.capacity(), 1);
        assert_eq!(r.latest(), Some(&'b'));
    }
}
