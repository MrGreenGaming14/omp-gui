//! Feeds read until they end: broadcast and watch channels, queues and
//! session writers.

use tokio::sync::{broadcast, mpsc, watch};

/// A feed read item by item until it ends.
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
