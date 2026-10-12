//! Every author flow on an EVM vault chain: paying to publish, activating,
//! and the controls of a live agent (its tank, auto-fill, allowances, idle
//! floor, the unsigned tank escape, retiring), as small state machines over
//! the same ports as the depositor's flows. See [`flow`] for the rules each
//! keeps.
//!
//! - [`archive`]: a packed project read before it is uploaded.
//! - [`form`]: typed text checked and turned into what the contracts take.
//! - [`read`]: what the flows and pages read from the chain.
//! - [`run`]: starting a flow from a page, in the browser.

pub mod archive;
pub mod error;
pub mod flow;
pub mod form;
pub mod read;
pub mod run;

#[cfg(test)]
mod sim;

pub use error::AuthorError;
pub use flow::{Activation, Outcome, Prompt, Spender, Step, Usage};
