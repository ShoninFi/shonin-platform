//! For tests only: the stand-in venue again, at its own address.
//!
//! A station takes one driver per venue program, so a station with several
//! drivers needs as many venues. This is the stand-in venue's own code,
//! unchanged, built as another program: everything it does is derived from
//! the program's address, so it is a second venue in every respect, with its
//! own markets, accounts and dollars.

include!(concat!(env!("OUT_DIR"), "/program_id.rs"));

include!("../../shonin-local-venue/src/venue.rs");
