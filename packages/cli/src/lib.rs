//! The `shonin` command, as a library, so the development chain can publish
//! through exactly the flow an author uses rather than around it.

pub mod publish;

pub use publish::{Published, publish};
