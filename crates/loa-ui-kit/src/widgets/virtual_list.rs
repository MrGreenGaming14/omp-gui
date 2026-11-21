//! Virtual lists: item heights with logarithmic prefix sums, so a list finds the items under a
//! scroll offset and its spacer sizes without walking every item; plus the scroll math.

use std::ops::Range;

/// Heights of every item, in order, backed by a Fenwick tree. Sums are `f64` so a long list
/// (millions of pixels) keeps sub-pixel precision.
#[derive(Debug, Default)]
pub struct Heights {
    values: Vec<f32>,
    /// 1-based Fenwick tree: `tree[i - 1]` covers items `(i - lowbit(i), i]`.
    tree: Vec<f64>,
}

fn lowbit(i: usize) -> usize { i.isolate_lowest_one() }

impl Heights {
    pub fn len(&self) -> usize { self.values.len() }

    pub fn is_empty(&self) -> bool { self.values.is_empty() }

    pub fn get(&self, index: usize) -> Option<f32> { self.values.get(index).copied() }

    /// Sum of the first `count` heights.
    pub fn prefix(&self, count: usize) -> f64 {
        let (mut i, mut sum) = (count.min(self.values.len()), 0.0);
        while i > 0 {
            sum += self.tree[i - 1];
            i -= lowbit(i);
        }
        sum
    }

    pub fn total(&self) -> f64 { self.prefix(self.values.len()) }

    pub fn push(&mut self, height: f32) {
        let i = self.values.len() + 1;
        // Node `i` covers `(i - lowbit(i), i]`: the new value plus the values before it in that span.
        let covered = self.prefix(i - 1) - self.prefix(i - lowbit(i));
        self.values.push(height);
        self.tree.push(covered + f64::from(height));
    }

    /// Keeps the first `len` heights; the remaining nodes only cover items before them, so stay valid.
    pub fn truncate(&mut self, len: usize) {
        self.values.truncate(len);
        self.tree.truncate(len);
    }

    /// Replaces the height at `index`; out of range is ignored.
    pub fn set(&mut self, index: usize, height: f32) {
        let Some(old) = self.values.get_mut(index) else { return };
        let delta = f64::from(height) - f64::from(*old);
        *old = height;
        let mut i = index + 1;
        while i <= self.tree.len() {
            self.tree[i - 1] += delta;
            i += lowbit(i);
        }
    }

    /// Replaces `range` with `new`: a tail splice costs only the new items, a middle one a rebuild.
    pub fn splice(&mut self, range: Range<usize>, new: impl IntoIterator<Item = f32>) {
        if range.end >= self.values.len() {
            self.truncate(range.start);
            new.into_iter().for_each(|h| self.push(h));
            return;
        }
        self.values.splice(range, new);
        let n = self.values.len();
        self.tree = self.values.iter().map(|v| f64::from(*v)).collect();
        for i in 1..=n {
            if i + lowbit(i) <= n {
                self.tree[i + lowbit(i) - 1] += self.tree[i - 1];
            }
        }
    }

    /// Index of the item containing position `y`, clamped to the last; `None` when empty.
    pub fn find(&self, y: f64) -> Option<usize> {
        let n = self.values.len();
        if n == 0 { return None }
        let (mut pos, mut rest, mut step) = (0, y, n.next_power_of_two());
        while step > 0 {
            if pos + step <= n && self.tree[pos + step - 1] <= rest {
                pos += step;
                rest -= self.tree[pos - 1];
            }
            step >>= 1;
        }
        Some(pos.min(n - 1))
    }

    /// The items overlapping `offset..offset + height`, widened by `overscan` on both sides.
    pub fn window(&self, offset: f32, height: f32, overscan: f32) -> Range<usize> {
        let Some(first) = self.find(f64::from(offset - overscan).max(0.0)) else { return 0..0 };
        first..self.find(f64::from(offset + height + overscan)).unwrap_or(first) + 1
    }
}

/// The scroll offset that brings `top..bottom` into a viewport of `height` scrolled to
/// `offset`, or `None` when it is already visible.
pub fn scroll_target(offset: f32, height: f32, top: f32, bottom: f32) -> Option<f32> {
    if top < offset {
        Some(top)
    } else if bottom > offset + height {
        Some(bottom - height)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_sums_stay_exact_through_push_set_truncate_and_splice() {
        let (mut heights, mut model) = (Heights::default(), Vec::new());
        for i in 0..37 {
            heights.push(10.0 + (i % 7) as f32 * 3.0);
            model.push(10.0 + (i % 7) as f32 * 3.0);
        }
        heights.set(5, 100.0);
        model[5] = 100.0;
        heights.truncate(30);
        model.truncate(30);
        heights.splice(10..12, [1.0, 2.0, 3.0]);
        model.splice(10..12, [1.0, 2.0, 3.0]);
        heights.splice(29..31, [7.0]);
        model.splice(29..31, [7.0]);
        (0..5).for_each(|_| heights.push(4.0));
        model.extend([4.0; 5]);
        for count in 0..=model.len() {
            assert_eq!(heights.prefix(count), model[..count].iter().map(|v| f64::from(*v)).sum::<f64>(), "prefix {count}");
        }
    }

    #[test]
    fn find_window_and_scroll_target() {
        let mut heights = Heights::default();
        [10.0, 20.0, 30.0].into_iter().for_each(|h| heights.push(h));
        let found = [(0.0, 0), (9.9, 0), (10.0, 1), (29.0, 1), (30.0, 2), (1e9, 2)];
        found.iter().for_each(|&(y, i)| assert_eq!(heights.find(y), Some(i), "{y}"));
        assert_eq!((Heights::default().find(0.0), heights.window(12.0, 5.0, 0.0), heights.window(0.0, 5.0, 40.0)), (None, 1..2, 0..3));
        assert_eq!((scroll_target(50.0, 100.0, 40.0, 60.0), scroll_target(50.0, 100.0, 140.0, 170.0), scroll_target(50.0, 100.0, 60.0, 70.0)), (Some(40.0), Some(70.0), None));
    }
}
