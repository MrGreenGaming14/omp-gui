//! Async scheduling helpers on tokio time, so paused-clock tests drive them.

use std::collections::HashMap;
use std::hash::Hash;
use std::time::Duration;
use tokio::time::Instant;

/// Per-key deadlines: each [`touch`](Self::touch) pushes the key out by the window,
/// so a burst of touches fires the key once, after it goes quiet.
#[derive(Debug)]
pub struct KeyedDebouncer<K> {
    window: Duration,
    due: HashMap<K, Instant>,
}

impl<K: Eq + Hash + Ord + Clone> KeyedDebouncer<K> {
    pub fn new(window: Duration) -> Self { Self { window, due: HashMap::new() } }

    pub fn touch(&mut self, key: K) { self.due.insert(key, Instant::now() + self.window); }

    /// The earliest pending deadline.
    pub fn next(&self) -> Option<Instant> { self.due.values().min().copied() }

    pub fn is_empty(&self) -> bool { self.due.is_empty() }

    /// Removes and returns, in key order, the keys due by `now` (every key when `all`).
    pub fn take_due(&mut self, now: Instant, all: bool) -> Vec<K> {
        let mut ready: Vec<K> = self.due.iter().filter(|(_, at)| all || **at <= now).map(|(k, _)| k.clone()).collect();
        ready.sort();
        self.due.retain(|k, _| !ready.contains(k));
        ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn touches_push_the_deadline_out() {
        let mut d = KeyedDebouncer::new(Duration::from_secs(2));
        d.touch("a");
        tokio::time::advance(Duration::from_secs(1)).await;
        d.touch("a");
        d.touch("b");
        tokio::time::advance(Duration::from_millis(1500)).await;
        assert!(d.take_due(Instant::now(), false).is_empty());
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(d.take_due(Instant::now(), false), ["a", "b"]);
        d.touch("c");
        assert_eq!((d.take_due(Instant::now(), true), d.is_empty()), (vec!["c"], true));
    }
}
