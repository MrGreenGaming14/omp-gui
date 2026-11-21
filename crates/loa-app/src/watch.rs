//! Subscriptions over shared feeds: broadcast and watch channels, queues and
//! session writers.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, Mutex};

use iced::Subscription;
use iced::futures::{Stream, stream};
use loa_bus::{Policy, SessionId};
use loa_runtime::feed::Source;
use loa_runtime::session::{Feed, Sessions, WriterFeed};
use loa_util::LockExt;

/// Undelivered patches a session feed may hold before it resubscribes.
const FEED_CAPACITY: usize = 4096;

/// A subscription key naming a shared value by its address: clones of one
/// `Arc` are the same feed, a new value is a new one.
pub struct PtrKey<'a, T: ?Sized>(pub &'a Arc<T>);

impl<T: ?Sized> Hash for PtrKey<'_, T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::ptr::hash(Arc::as_ptr(self.0).cast::<()>(), state);
    }
}

/// The items of the feed `open` returns, as `map` turns them into messages
/// (`None` skips one).
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

/// One feed per open session: the writer's DOM, then every patch.
pub fn sessions(sessions: &Sessions) -> Subscription<(SessionId, Feed)> {
    Subscription::batch(sessions.iter().map(|session| {
        let writer = Arc::clone(session.writer());
        let open = move || WriterFeed::new(writer, Policy::coalescing(FEED_CAPACITY));
        watch(PtrKey(session.writer()), open, |feed| (!matches!(feed, Feed::Ended)).then_some(feed)).with(session.id().clone())
    }))
}
