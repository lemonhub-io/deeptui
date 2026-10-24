//! The pure heart of deeptui: what a conversation *is*, and how one turn of it
//! unfolds — with no IO at all.
//!
//! Nothing in this crate can touch the network, the disk, a terminal or the
//! clock; its manifest does not even depend on an async runtime. Every outside
//! fact arrives as a value ([`Delta`], a tool result, a timestamp) and every
//! decision leaves as one ([`Next`], [`Event`]). That is what makes the agent
//! loop a plain state machine ([`Turn`]) that tests can drive line by line, and
//! what lets each frontend be nothing but a fold over [`Event`]s.

pub mod event;
pub mod history;
pub mod ledger;
pub mod msg;
pub mod schedule;
pub mod tariff;
pub mod text;
pub mod turn;

pub use event::{Ask, Choice, Event, Outcome, Projection, Receipt, Usage};
pub use ledger::Ledger;
pub use msg::{FnCall, Msg, Role, ToolCall};
pub use schedule::Effect;
pub use tariff::Tariff;
pub use turn::{Delta, Limits, Next, StreamEnd, ToolOutput, Turn};
