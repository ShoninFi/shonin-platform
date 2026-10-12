//! Every depositor flow on an EVM vault chain, as a small state machine over
//! two ports: a reader of the vault's chain and the connected wallet. See
//! [`flow`] for the rules every flow keeps.

pub mod author;
pub mod error;
pub mod flow;
pub mod port;
pub mod read;

#[cfg(test)]
mod sim;

pub use error::{Revert, TxError};
pub use flow::{Cancel, Countdown, Ctx, Fallback, Outcome, Pace, Prompt, Step};
pub use port::{Clock, Reader, Receipt, Signer, StatementRequest, TxRequest};
