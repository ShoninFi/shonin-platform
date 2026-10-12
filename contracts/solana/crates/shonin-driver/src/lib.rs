//! The interface between a station and the on-chain half of a driver.
//!
//! A driver is a program nobody can change, approved in the registry under a
//! name, and pinned by every station created while it is the approval in
//! force. It holds nothing and acts on nothing: it only answers, through the
//! program's return data, and the station makes every venue call itself. A
//! driver is never handed a signature: the station gives it every account
//! read-only and none as a signer.
//!
//! **The station's authority at the venue.** A station has an authority at
//! each venue it pins, a program-derived address of the station program kept
//! for that venue alone, and signs every call to the venue as that authority
//! and as nothing else. The authority is the master of the station's account
//! at the venue and owns the station's token accounts there, so a venue's
//! code, whatever it becomes, holds a signature no other venue's accounts
//! accept. Wherever this interface says "the station's" about a venue
//! account, it means its authority's at that venue. The authority's dollar
//! account holds the dollars the station sends it for a trade, and the
//! station takes back whatever is there after every call, so between calls
//! it holds nothing a driver should count: a driver values the station's
//! position at the venue, never that account.
//!
//! **What an allowlist must pin.** A rule can say an account must be the
//! authority's own ([`Own::Station`], [`Own::StationDollars`],
//! [`Own::StationToken`], [`Own::StationAddress`], and for a venue that
//! holds positions as tokens [`Own::StationMint`],
//! [`Own::StationPositionToken`] and [`Own::StationPositionHeld`]) or one of
//! a set of accounts the driver names when the station is created
//! ([`Own::Fixed`]).
//! Whatever a rule leaves unconstrained, the executor's key chooses. So a
//! driver must pin every account through which the station's money could
//! reach someone else: the venue's own settings and vaults, and on a venue
//! where anyone can create a pool or a market, the pools, markets and mints
//! the strategy trades (an Orca driver pins its whirlpools and their mints,
//! and every token account of the station's by the mint it must hold). A
//! venue whose operator alone lists markets may leave them open, as the
//! stand-in venue does: what a market it lists after the station was
//! created can do is that operator's power, which a station on that venue
//! accepts with the venue.
//!
//! **Positions held as tokens.** A venue may record a position as a token
//! the holder keeps (an NFT, as Orca does) and let whoever holds that token,
//! or is its delegate, act on the position. Its allowlist then pins three
//! things more: that a new position's mint is created at one of the
//! authority's position mints ([`Own::StationMint`]), the token account the
//! position's token lands in ([`Own::StationPositionToken`]), and, wherever
//! the venue takes a token account as proof of a position, that the account
//! is the authority's own and holds one of those mints
//! ([`Own::StationPositionHeld`]). So the authority acts only on positions at
//! its own slots, never on one a stranger delegated to it, and a driver
//! finds every position the station holds by looking at each slot.
//!
//! Every instruction is laid out as an Anchor instruction: the discriminator
//! below, then its arguments in Borsh. Each answer is Borsh, whole (trailing
//! bytes are refused), and fits the runtime's 1,024 bytes of return data.
//!
//! **The accounts a driver is given.** Index 0 is the station's authority at
//! the driver's venue, index 1 that authority's dollar account; from index 2
//! on come the accounts the driver documents for its venue, in its order.
//! Whoever asks the station for a value or a close supplies those, and the
//! driver must derive or check every
//! one it reads and require every one it needs, so nobody can leave a
//! position out or pass a look-alike: a driver that can't be sure of what it
//! reads fails rather than answers.
//!
//! **Prepared instructions.** A close or a sweep is answered with venue
//! instructions as data: the program and each account as an index into the
//! accounts the driver was given, so an instruction costs a byte per account.
//! The station rebuilds each one, checks it against the allowlist it pinned
//! from this driver at creation, as it checks the executor's own, and makes
//! the call itself. Only the station's authority at the venue ever signs. A
//! sweep brings the dollars to the authority's dollar account (index 1), and
//! the station moves them on to its own.
//!
//! **What a driver is approved for.** The registry approves a driver only if
//! all of these hold:
//!
//! - its allowlist pins every account through which the station's money could
//!   reach someone else (above);
//! - it answers every instruction in [`discriminator`], `unwind` included:
//!   a stopped station's unwind asks it, and a driver without it leaves its
//!   venue's money where it is;
//! - its venue holds the station's account to at most [`MAX_MARKETS`]
//!   markets, and its close and its unwind each fit [`COMPUTE_PER_DRIVER`];
//! - its venue never pays the authority's token accounts outside a station
//!   call: no close a keeper fills later, no crank, no remainder of a
//!   liquidation paid there afterwards. The station counts only what reaches
//!   the authority's dollar account during its own call, and moves only that;
//!   money that arrives any other way is counted by nobody and the station
//!   never moves it, so it is lost to the vault. A venue that settles only
//!   into the station's account at the venue, for a later call to withdraw,
//!   meets this.

use anchor_lang::prelude::*;

/// A whole position, or all of an account: fractions are out of this.
pub const WAD: u64 = 1_000_000_000_000_000_000;
/// Basis points in one.
pub const BPS: u64 = 10_000;
/// The most return data a program can leave, which every answer must fit.
pub const MAX_RETURN_DATA: usize = 1_024;
/// The most instructions an allowlist names for one venue.
pub const MAX_RULES: usize = 8;
/// The most account positions one instruction's rule constrains.
pub const MAX_POSITIONS: usize = 6;
/// The longest instruction discriminator a rule matches on.
pub const MAX_DISCRIMINATOR: usize = 8;
/// The longest seed prefix a [`Own::StationAddress`] names. The allowlist
/// limits are set together so that the largest allowlist a station accepts
/// (every rule at its longest discriminator, every position a seed this long)
/// still fits the return data a driver answers in; this is the longest seed
/// for which it does.
pub const MAX_SEED: usize = 11;
/// The most distinct seed prefixes an allowlist's [`Own::StationAddress`]
/// positions name: a station works out each address once, when it pins the
/// allowlist, and keeps it in its record.
pub const MAX_STATION_ADDRESSES: usize = 4;
/// The most accounts an allowlist pins in its [`Allowlist::fixed`] sets, all
/// sets together. The answer an allowlist comes in must also fit the return
/// data, which bounds the rules and the pinned accounts together: an
/// allowlist with every rule at its largest has room for none.
pub const MAX_FIXED: usize = 16;
/// The most codes a driver's restricted list holds: as many 32-byte codes as
/// fit the return data.
pub const MAX_RESTRICTED: usize = 31;
/// The most markets one driver's venue account may hold, and so the most
/// orders one close through it places: a fractional close asks every driver
/// in one transaction, so what one may hold bounds what a close costs. A
/// driver is approved only for a venue that holds the station's account to
/// this many (the stand-in venue's own limit is exactly this), and a station
/// credits no driver with more orders than this when it allows for rounding.
pub const MAX_MARKETS: usize = 8;
/// The most compute one driver's part of a close may take at the most
/// markets its account may hold: asking it for its close (which carries its
/// valuation) and sweep, making the venue instructions they prepare, asking
/// its value afterwards, and the station's own work around them. A station
/// declares at most as many drivers as fit, each at this, in the 1,400,000
/// units a transaction may ask for less the 200,000 left to the caller, so a
/// close through every one of them always fits; a driver is approved only if
/// its close fits this (and only if its venue never pays the authority's
/// token accounts outside a station call: see the crate's documentation for
/// every condition of approval). The stand-in venue's driver takes about 358,000 at
/// eight markets with funding owed on every position (an unwinder's
/// fractional close through three of them measured 1,071,370 units, its
/// registry reads included), which leaves it about a tenth of this to
/// spare.
pub const COMPUTE_PER_DRIVER: u64 = 400_000;
/// The most position mints ([`Own::StationMint`]) an allowlist gives the
/// authority at one venue, and so the most positions it can hold there at
/// once: a driver looks at every slot each time it values the station.
pub const MAX_POSITION_MINTS: usize = 8;
/// The first seed of a position mint's address under the station program.
pub const POSITION_MINT_SEED: &[u8] = b"position-mint";

/// The authority's position mint at `slot`, and its bump: the station
/// program's address with seeds `[POSITION_MINT_SEED, authority, [slot]]`.
pub fn position_mint(station_program: &Pubkey, authority: &Pubkey, slot: u8) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[POSITION_MINT_SEED, authority.as_ref(), &[slot]],
        station_program,
    )
}

/// Each instruction's first eight bytes, `sha256("global:<name>")[..8]`, as
/// Anchor lays them out.
pub mod discriminator {
    /// `allowlist()`, answered with an [`super::Allowlist`].
    pub const ALLOWLIST: [u8; 8] = [0, 51, 50, 227, 108, 194, 231, 209];
    /// `restricted()`, answered with a `Vec<[u8; 32]>`.
    pub const RESTRICTED: [u8; 8] = [186, 193, 18, 126, 183, 177, 13, 70];
    /// `describe()`, answered with a [`super::Description`].
    pub const DESCRIBE: [u8; 8] = [230, 44, 148, 126, 188, 197, 69, 232];
    /// `value(band_bps: u16)`, answered with a [`super::Valuation`].
    pub const VALUE: [u8; 8] = [88, 248, 79, 196, 62, 175, 241, 16];
    /// `close(fraction: u64, band_bps: u16)`, answered with a
    /// [`super::Close`].
    pub const CLOSE: [u8; 8] = [98, 165, 201, 177, 108, 65, 206, 96];
    /// `sweep(amount: u64)`, answered with a [`super::Plan`].
    pub const SWEEP: [u8; 8] = [40, 23, 234, 175, 14, 61, 154, 177];
    /// `unwind(band_bps: u16)`, answered with a [`super::Close`]: a stopped
    /// station's close of everything inside `band_bps`. A driver whose venue
    /// can't sell it all in one transaction may plan less, its valuation
    /// then what that plan fetches, and the next unwind goes on from there.
    pub const UNWIND: [u8; 8] = [66, 99, 1, 75, 47, 72, 63, 85];
}

/// What a venue instruction may do, for when a driver is revoked: the
/// registry's admin can revoke a pinned driver at once, and the station then
/// stops trading through it and keeps closing through it.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// Puts money at risk or into the venue: refused once the driver is
    /// revoked.
    Trade,
    /// Only brings money back to the station: allowed whatever the registry
    /// says now.
    Exit,
}

/// What must stand at one account position of a venue instruction. Each
/// "station" here is the station's authority at the driver's venue, the only
/// address the station signs a call to it with.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub enum Own {
    /// The station's authority at the venue, which signs there and nowhere
    /// else.
    Station,
    /// The authority's dollar account.
    StationDollars,
    /// The authority's associated token account for the mint the account
    /// holds, under the token program that owns it. With a set, that mint
    /// must be one of the accounts in [`Allowlist::fixed`] at that index;
    /// without one, any mint.
    StationToken(Option<u8>),
    /// The venue's program-derived address for the authority: seeds
    /// `[prefix, authority]` under the venue program.
    StationAddress(Vec<u8>),
    /// One of the accounts in [`Allowlist::fixed`] at this index: a venue's
    /// settings or vault, a pool, a market, pinned when the station is
    /// created.
    Fixed(u8),
    /// One of the authority's first `n` position mints (at most
    /// [`MAX_POSITION_MINTS`]): the station program's addresses with seeds
    /// `[POSITION_MINT_SEED, authority, [slot]]`, which the station works out
    /// when it pins the allowlist. The station signs for the one standing
    /// here, so that a venue which creates a position's mint at the address
    /// of a signer (as Orca does) creates it at one of these, and a driver can
    /// find every position the authority can ever hold by looking at each
    /// slot. A mint the venue closes when its position closes frees its slot
    /// for the next. Nothing is held at such an address: its signature lets a
    /// venue create an account there and nothing else.
    StationMint(u8),
    /// The authority's associated token account, under either token
    /// program, for the mint standing at this index of the same instruction,
    /// which the rule must pin as a [`Own::StationMint`]: a position's token
    /// account. Unlike the authority's other token accounts, the call may
    /// create it or close it; if it is still there afterwards it must still
    /// be the authority's alone.
    StationPositionToken(u8),
    /// A token account, under either token program, that the authority owns
    /// and that holds one of its position mints: the proof a venue takes that
    /// a position is the authority's. Never one a stranger made the authority
    /// a delegate of, nor a position at an address no driver looks at.
    StationPositionHeld,
}

/// One constrained position: the account at `index` must be `own`.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct Position {
    pub index: u8,
    pub own: Own,
}

/// One venue instruction a station may make: those whose data starts with
/// `discriminator`, with the authority's own accounts at `positions`. The
/// authority's address may stand only at a position marked [`Own::Station`],
/// and the station's own address nowhere.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub discriminator: Vec<u8>,
    pub class: Class,
    pub positions: Vec<Position>,
}

/// Everything a station may ever call at this driver's venue: one program,
/// the instructions there, and the venue's accounts its rules pin. A station
/// pins it at creation, and nothing changes it afterwards.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct Allowlist {
    pub venue: Pubkey,
    pub rules: Vec<Rule>,
    /// Sets of accounts, each named by its index from [`Own::Fixed`] and
    /// [`Own::StationToken`]: a set of one pins a single account, a larger
    /// set allows any of its accounts (one of the named pools, say). At most
    /// [`MAX_FIXED`] accounts in all, and no set empty.
    pub fixed: Vec<Vec<Pubkey>>,
}

/// What a station holds at the venue, at the venue's marks and at what a
/// sale would fetch, in the dollar's units; and whether any market it holds
/// has its mark, bid or ask further from the venue's own price than the band
/// asked about, on either side.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Valuation {
    pub mark: u64,
    pub exit: u64,
    pub outside: bool,
}

/// One account of a prepared instruction: an index into the accounts the
/// driver was given, and whether the instruction writes it.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Meta {
    pub index: u8,
    pub writable: bool,
}

/// One venue instruction a driver prepares: its program and accounts as
/// indexes into the accounts the driver was given.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct Prepared {
    pub program: u8,
    pub accounts: Vec<Meta>,
    pub data: Vec<u8>,
}

/// The instructions a driver prepares for a close or a sweep. `outside` is
/// true, with no instructions, when a market is outside the band: nothing is
/// closed then, so a leaver is never sold through a market that has run away.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub outside: bool,
    pub instructions: Vec<Prepared>,
    /// How many orders the instructions place, one per market a close
    /// reduces; zero for a sweep. A venue rounds each order's proceeds down,
    /// so a close may come home up to a dollar-unit short per order and still
    /// be whole; the station allows that much, for at most [`MAX_MARKETS`]
    /// orders, and no more.
    pub orders: u8,
}

/// A driver's answer to `close`: the plan, and the valuation of the account
/// at the same band, read from the same accounts the plan was prepared from.
/// The station takes what the fraction is worth from this valuation rather
/// than asking `value` separately before the close, so a close costs one
/// valuation per driver before anything is sold, not two; it still values
/// the account again after the close, to check the stayers keep their part.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Close {
    pub value: Valuation,
    pub plan: Plan,
}

/// Who stops a vault's keys sending money anywhere but its own accounts.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Custody {
    Venue,
    Station,
}

/// Who closes positions when someone leaves.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitPath {
    Contract,
    Unwinder,
}

/// How a station's holdings at the venue are valued.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValuationKind {
    OnChain,
    AttestedRead,
}

/// What a driver says about itself.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Description {
    /// The name `shonin.toml` writes, zero-padded.
    pub name: [u8; 32],
    pub custody: Custody,
    pub exit_path: ExitPath,
    pub valuation: ValuationKind,
}

/// An instruction's data: its discriminator, then its arguments in Borsh.
pub fn data(discriminator: [u8; 8], args: &impl AnchorSerialize) -> Vec<u8> {
    let mut data = discriminator.to_vec();
    args.serialize(&mut data)
        .expect("serialising into a vector can't fail");
    data
}

/// `amount * numerator / denominator`, rounded down, in 128 bits, so the
/// product never overflows: a dollar amount times a fraction of [`WAD`] or a
/// number of basis points.
pub fn mul_div(amount: u64, numerator: u64, denominator: u64) -> u64 {
    debug_assert!(denominator != 0);
    ((amount as u128 * numerator as u128) / denominator as u128) as u64
}

/// The same, rounded up.
pub fn mul_div_up(amount: u64, numerator: u64, denominator: u64) -> u64 {
    debug_assert!(denominator != 0);
    (amount as u128 * numerator as u128).div_ceil(denominator as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The largest allowlist the limits allow: every rule at its longest
    /// discriminator, constraining every position with the longest seed.
    fn largest_allowlist(seed: usize) -> Allowlist {
        let rule = |i: u8| Rule {
            discriminator: vec![i; MAX_DISCRIMINATOR],
            class: Class::Trade,
            positions: (0..MAX_POSITIONS as u8)
                .map(|index| Position {
                    index,
                    own: Own::StationAddress(vec![0; seed]),
                })
                .collect(),
        };
        Allowlist {
            venue: Pubkey::default(),
            rules: (0..MAX_RULES as u8).map(rule).collect(),
            fixed: Vec::new(),
        }
    }

    #[test]
    fn the_largest_rules_a_station_accepts_fit_an_answer() {
        let fits = |seed| largest_allowlist(seed).try_to_vec().unwrap().len() <= MAX_RETURN_DATA;
        assert!(fits(MAX_SEED));
        // And the seed is as long as it can be.
        assert!(!fits(MAX_SEED + 1));
    }

    #[test]
    fn every_account_an_allowlist_may_pin_fits_an_answer_beside_a_rule() {
        let rule = Rule {
            discriminator: vec![0; MAX_DISCRIMINATOR],
            class: Class::Trade,
            positions: (0..MAX_POSITIONS as u8)
                .map(|index| Position {
                    index,
                    own: Own::Fixed(index),
                })
                .collect(),
        };
        let allowlist = Allowlist {
            venue: Pubkey::default(),
            rules: vec![rule],
            fixed: (0..MAX_FIXED).map(|_| vec![Pubkey::default()]).collect(),
        };
        assert!(allowlist.try_to_vec().unwrap().len() <= MAX_RETURN_DATA);
    }

    #[test]
    fn the_largest_restricted_list_fits_an_answer() {
        let codes = vec![[0u8; 32]; MAX_RESTRICTED];
        assert!(codes.try_to_vec().unwrap().len() <= MAX_RETURN_DATA);
        let codes = vec![[0u8; 32]; MAX_RESTRICTED + 1];
        assert!(codes.try_to_vec().unwrap().len() > MAX_RETURN_DATA);
    }

    #[test]
    fn a_fraction_of_the_largest_amount_never_overflows() {
        assert_eq!(mul_div(u64::MAX, WAD, WAD), u64::MAX);
        assert!(mul_div(u64::MAX, BPS - 1, BPS) < u64::MAX);
        assert_eq!(mul_div_up(1, 1, 3), 1);
        assert_eq!(mul_div(1, 1, 3), 0);
    }
}
