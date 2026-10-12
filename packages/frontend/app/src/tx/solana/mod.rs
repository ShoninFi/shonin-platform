//! Every depositor and author flow on a Solana vault, as small state
//! machines over two ports: a reader of the cluster and the connected
//! Solana wallet. Instructions come only from `chain::solana_ix`, whose
//! builders the chain crate checks byte for byte against the programs. See
//! [`flow`] for the rules every flow keeps.

pub mod author;
pub mod error;
pub mod flow;
pub mod port;
pub mod read;
pub mod run;

#[cfg(test)]
mod live;
#[cfg(test)]
mod sim;

pub use error::SolError;
pub use flow::{Outcome, Prompt, Step};
pub use run::{SolAction, SolRunning, SolVault};
