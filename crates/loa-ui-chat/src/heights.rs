//! Item heights with logarithmic prefix sums, so the virtual list finds the
//! items under a scroll offset and the spacer sizes without walking the
//! transcript.

use std::ops::Range;

/// Heights of every transcript item, in order, backed by a Fenwick tree.
///
/// Sums are `f64` so a long transcript (millions of pixels) keeps sub pixel
/// precision.
#[derive(Debug, Default)]
pub(crate) struct Heights {
    values: Vec<f32>,
    /// 1-based Fenwick tree: `tree[i - 1]` covers items `(i - lowbit(i), i]`.
    tree: Vec<f64>,
}

fn lowbit(i: usize) -> usize {
    i.isolate_lowest_one()
}

impl Heights {
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.values.len()
    }

    pub(crate) fn get(&self, index: usize) -> Option<f32> {
        self.values.get(index).copied()
    }

    /// Sum of the first `count` heights.
    pub(crate) fn prefix(&self, count: usize) -> f64 {
        let mut i = count.min(self.values.len());
        let mut sum = 0.0;
        while i > 0 {
            sum += self.tree[i - 1];
            i -= lowbit(i);
        }
        sum
    }

    pub(crate) fn total(&self) -> f64 {
        self.prefix(self.values.len())
    }

    /// Appends one height.
    pub(crate) fn push(&mut self, height: f32) {
        let i = self.values.len() + 1;
        // Node `i` covers `(i - lowbit(i), i]`: the new value plus the
        // values already stored before it in that span.
        let covered = self.prefix(i - 1) - self.prefix(i - lowbit(i));
        self.values.push(height);
        self.tree.push(covered + f64::from(height));
    }

    /// Keeps the first `len` heights. Fenwick nodes only cover items at or
    /// before their own index, so the remaining nodes stay valid.
    pub(crate) fn truncate(&mut self, len: usize) {
        self.values.truncate(len);
        self.tree.truncate(len);
    }

    /// Replaces the height at `index`. Out of range indices are ignored.
    pub(crate) fn set(&mut self, index: usize, height: f32) {
        let Some(old) = self.values.get_mut(index) else {
            return;
        };
        let delta = f64::from(height) - f64::from(*old);
        *old = height;
        let mut i = index + 1;
        while i <= self.tree.len() {
            self.tree[i - 1] += delta;
            i += lowbit(i);
        }
    }

    /// Replaces the heights in `range` with `new`. A splice at the tail costs
    /// only the new items; one in the middle rebuilds the tree in linear time.
    pub(crate) fn splice(&mut self, range: Range<usize>, new: impl IntoIterator<Item = f32>) {
        if range.end >= self.values.len() {
            self.truncate(range.start);
            for height in new {
                self.push(height);
            }
            return;
        }
        self.values.splice(range, new);
        self.rebuild();
    }

    fn rebuild(&mut self) {
        let n = self.values.len();
        self.tree.clear();
        self.tree.extend(self.values.iter().map(|v| f64::from(*v)));
        for i in 1..=n {
            let parent = i + lowbit(i);
            if parent <= n {
                let child = self.tree[i - 1];
                self.tree[parent - 1] += child;
            }
        }
    }

    /// Index of the item that contains vertical position `y`, clamped to the
    /// last item. `None` when there are no items.
    pub(crate) fn find(&self, y: f64) -> Option<usize> {
        let n = self.values.len();
        if n == 0 {
            return None;
        }
        let mut pos = 0;
        let mut rest = y;
        let mut step = n.next_power_of_two();
        while step > 0 {
            let next = pos + step;
            if next <= n && self.tree[next - 1] <= rest {
                pos = next;
                rest -= self.tree[next - 1];
            }
            step >>= 1;
        }
        Some(pos.min(n - 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naive_prefix(values: &[f32], count: usize) -> f64 {
        values[..count].iter().map(|v| f64::from(*v)).sum()
    }

    #[test]
    fn prefix_sums_stay_exact_through_push_set_truncate_and_splice() {
        let mut heights = Heights::default();
        let mut model: Vec<f32> = Vec::new();
        for i in 0..37 {
            let h = 10.0 + (i % 7) as f32 * 3.0;
            heights.push(h);
            model.push(h);
        }
        heights.set(5, 100.0);
        model[5] = 100.0;
        heights.truncate(30);
        model.truncate(30);
        heights.splice(10..12, [1.0, 2.0, 3.0]);
        model.splice(10..12, [1.0, 2.0, 3.0]);
        heights.splice(29..31, [7.0]);
        model.splice(29..31, [7.0]);
        for _ in 0..5 {
            heights.push(4.0);
        }
        model.extend([4.0; 5]);
        for count in 0..=model.len() {
            assert_eq!(
                heights.prefix(count),
                naive_prefix(&model, count),
                "prefix {count}"
            );
        }
    }

    #[test]
    fn find_returns_the_item_under_an_offset() {
        let mut heights = Heights::default();
        for h in [10.0, 20.0, 30.0] {
            heights.push(h);
        }
        assert_eq!(heights.find(0.0), Some(0));
        assert_eq!(heights.find(9.9), Some(0));
        assert_eq!(heights.find(10.0), Some(1));
        assert_eq!(heights.find(29.0), Some(1));
        assert_eq!(heights.find(30.0), Some(2));
        assert_eq!(heights.find(1e9), Some(2));
        assert_eq!(Heights::default().find(0.0), None);
    }
}
