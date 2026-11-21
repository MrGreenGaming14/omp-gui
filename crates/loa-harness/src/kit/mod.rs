//! Protocol machinery every harness shares: stdio RPC with pluggable
//! framing, process handling, turn bookkeeping, the generic driven session,
//! native codecs and residual round-trip helpers.

pub mod ledger;
pub mod mapper;
pub mod native;
pub mod proc;
pub mod residual;
pub mod rpc;

pub use ledger::{EndEvent, OpenTurn, TurnLedger};
pub use mapper::{DrivenSession, EventMapper, HarnessOps};
pub use native::{NativeCodec, NativeSession};
pub use rpc::{Framing, Inbound, Incoming, RpcError, StdioRpc};
