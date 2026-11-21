//! The node's protocols over the wire codec.

pub mod control;
pub mod extra;
pub mod folder;
pub mod projection;
pub mod session;

use crate::node::{Inner, Node};
use crate::transport::{Connection, ProtocolHandler, Stream};
use crate::Result;
use async_trait::async_trait;
use std::sync::Weak;

/// Serves each accepted stream with `F` on its own task while the node lives.
pub(crate) struct Served<F>(pub Weak<Inner>, pub F);

#[async_trait]
impl<F, Fut> ProtocolHandler for Served<F>
where
    F: Fn(Node, Connection, Stream) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Result<()>> + Send + 'static,
{
    async fn handle(&self, conn: Connection) {
        while let Ok(stream) = conn.accept_stream().await {
            let Some(inner) = self.0.upgrade() else { return };
            let (serve, conn) = (self.1.clone(), conn.clone());
            tokio::spawn(async move {
                let peer = conn.remote();
                if let Err(error) = serve(Node(inner), conn, stream).await {
                    tracing::debug!(%peer, %error, "peer stream ended");
                }
            });
        }
    }
}
