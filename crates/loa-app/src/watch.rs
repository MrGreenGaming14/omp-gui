//! Subscriptions over shared feeds: broadcast and watch channels, queues and
//! session writers. A [`watch`] runs while its key is in the app's
//! subscription list; the feed it opened is dropped once the key is gone.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, Mutex};

use iced::Subscription;
use iced::futures::{Stream, stream};
use loa_util::LockExt;
use tokio::sync::{broadcast, mpsc, watch};

/// A subscription key naming a shared value by its address: clones of one
/// `Arc` are the same feed, a new value is a new one.
pub struct PtrKey<'a, T: ?Sized>(pub &'a Arc<T>);

impl<T: ?Sized> Hash for PtrKey<'_, T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::ptr::hash(Arc::as_ptr(self.0).cast::<()>(), state);
    }
}

/// A feed a [`watch`] reads until it ends.
pub trait Source: Send + 'static {
    type Item: Send + 'static;

    /// The next item; `None` once the feed ended.
    fn next(&mut self) -> impl Future<Output = Option<Self::Item>> + Send;
}

impl<T: Clone + Send + 'static> Source for broadcast::Receiver<T> {
    /// `Err(skipped)` after the receiver fell behind.
    type Item = Result<T, u64>;

    async fn next(&mut self) -> Option<Result<T, u64>> {
        match self.recv().await {
            Ok(item) => Some(Ok(item)),
            Err(broadcast::error::RecvError::Lagged(skipped)) => Some(Err(skipped)),
            Err(broadcast::error::RecvError::Closed) => None,
        }
    }
}

/// Each change of the watched value.
impl<T: Send + Sync + 'static> Source for watch::Receiver<T> {
    type Item = ();

    async fn next(&mut self) -> Option<()> {
        self.changed().await.ok()
    }
}

impl<T: Send + 'static> Source for mpsc::UnboundedReceiver<T> {
    type Item = T;

    async fn next(&mut self) -> Option<T> {
        self.recv().await
    }
}

/// No feed ends at once.
impl<S: Source> Source for Option<S> {
    type Item = S::Item;

    async fn next(&mut self) -> Option<S::Item> {
        match self {
            Some(source) => source.next().await,
            None => None,
        }
    }
}

/// The items of the feed `open` returns, as `map` turns them into messages
/// (`None` skips one). `open` runs once, when `key` first shows up among the
/// subscriptions; the feed is dropped when it no longer does.
pub fn watch<S: Source, M: Send + 'static>(
    key: impl Hash,
    open: impl FnOnce() -> S + Send + 'static,
    map: fn(S::Item) -> Option<M>,
) -> Subscription<M> {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    (map as usize).hash(&mut hasher);
    Subscription::run_with(Watch { key: hasher.finish(), open: Mutex::new(Some(Box::new(open))), map }, items::<S, M>)
}

struct Watch<S: Source, M> {
    key: u64,
    open: Mutex<Option<Box<dyn FnOnce() -> S + Send>>>,
    map: fn(S::Item) -> Option<M>,
}

impl<S: Source, M> Hash for Watch<S, M> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.key.hash(state);
    }
}

fn items<S: Source, M: Send + 'static>(watch: &Watch<S, M>) -> impl Stream<Item = M> + use<S, M> {
    let source = watch.open.locked().take().map(|open| open());
    let map = watch.map;
    stream::unfold(source, move |source| async move {
        let mut source = source?;
        loop {
            if let Some(message) = map(source.next().await?) {
                return Some((message, Some(source)));
            }
        }
    })
}
