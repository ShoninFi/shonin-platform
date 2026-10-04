//! The venue a vault's capital sits on, for local chains and tests.
//!
//! It stands where a real exchange stands, and keeps the property the
//! platform rests on: an account belongs to one **master**, and only the
//! master, signing, can move dollars into or out of it, and only from and to
//! a token account the master owns. On Solana the master is a station's
//! authority at this venue, an address of the station program kept for this
//! venue alone, and only the station program can sign for it, so every
//! instruction here reaches the venue through the station's guard. There are
//! no agent keys: nothing here is signed by a trading key, which is how a
//! venue the station calls directly (Orca, say) works too.
//!
//! It keeps no book. Every order is immediate-or-cancel against the market's
//! bid and ask, because the flows the platform depends on (entering,
//! reversing, closing a leaver's fraction inside an exit band) all cross the
//! spread.
//!
//! Sizes carry 18 decimals and every dollar amount the dollar's 6, so
//! `size * price / 1e18` is a dollar amount. A fill's dollars round against
//! whoever placed the order: a buy costs that amount rounded up, a sale
//! brings it in rounded down, so nobody gets size, or dollars, for nothing.
//! Cash is signed: buying a perp on margin leaves it negative, which is what
//! leverage is. An account lists the markets it holds now and forgets one as
//! soon as its position is flat, so whoever values it reads only open
//! positions.
//!
//! Everything is derived from the program's own address, so the same code
//! deployed at another address is another venue (the tests deploy copies, to
//! give a station a venue per driver).
//!
//! The program's code is in `venue.rs`, included here, so that the copies
//! the tests deploy build exactly this code at their own addresses.

include!(concat!(env!("OUT_DIR"), "/program_id.rs"));

include!("venue.rs");
