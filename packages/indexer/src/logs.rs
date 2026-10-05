//! Reading logs a page at a time, carrying on from where the last read
//! stopped.
//!
//! Providers cap how many blocks one `eth_getLogs` may span, and differ in
//! where: so no read asks for more than [`PAGE`] blocks, a page the node
//! refuses is halved until it is taken, and each stream of logs keeps the
//! block it has been read to, so a later refresh asks only for what is new.
//! A stream that can't be read keeps what it had and is read again from the
//! same block next time; it holds up nothing that doesn't depend on it.

use alloy::rpc::types::{Filter, Log};
use anyhow::Result;
use chain::Client;

/// The widest block range one read asks for.
pub const PAGE: u64 = 2_000;

/// The most pages one stream is read for in one refresh, so catching up on a
/// long history holds the rest of the record up for no longer than this; it
/// carries on at the next refresh.
pub const PAGES_PER_REFRESH: usize = 25;

/// Where a stream of logs has been read to: the next block to ask for.
#[derive(Clone, Copy, Debug)]
pub struct Cursor {
    next: u64,
}

impl Cursor {
    /// A stream read from `from` on.
    pub fn from(from: u64) -> Self {
        Self { next: from }
    }

    /// Whether everything up to `head` has been read.
    pub fn caught_up(&self, head: u64) -> bool {
        self.next > head
    }
}

/// What one read of a stream came to: the logs of every page taken, in
/// order, and the error that stopped it short, if one did. The cursor is
/// moved past every page taken, whether or not a later one failed.
pub struct Pages {
    pub logs: Vec<Log>,
    pub error: Option<anyhow::Error>,
}

/// The logs `filter` matches from the cursor up to `head`, a page at a time,
/// for at most [`PAGES_PER_REFRESH`] pages.
pub async fn read_on(chain: &Client, filter: &Filter, cursor: &mut Cursor, head: u64) -> Pages {
    let mut logs = Vec::new();
    let mut span = PAGE;
    let mut pages = 0;
    while cursor.next <= head && pages < PAGES_PER_REFRESH {
        let to = head.min(cursor.next + span - 1);
        match page(chain, filter, cursor.next, to).await {
            Ok(found) => {
                logs.extend(found);
                cursor.next = to + 1;
                pages += 1;
            }
            // Most likely a node that caps how wide a read may be.
            Err(_) if span > 1 => span = (span / 2).max(1),
            Err(error) => {
                return Pages {
                    logs,
                    error: Some(error),
                };
            }
        }
    }
    Pages { logs, error: None }
}

async fn page(chain: &Client, filter: &Filter, from: u64, to: u64) -> Result<Vec<Log>> {
    chain
        .logs(&filter.clone().from_block(from).to_block(to))
        .await
}
