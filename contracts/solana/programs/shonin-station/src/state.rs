//! What a station keeps, and the rules that read it.

use anchor_lang::prelude::*;
use shonin_common::DAY;
use shonin_driver::{Rule, BPS};

/// The program's settings: one per deployment.
pub const CONFIG_SEED: &[u8] = b"config";
/// One vault's station: seeds `[STATION_SEED, vault]`, so a vault has one
/// station, at an address anyone can work out, and only the vault can create
/// it.
pub const STATION_SEED: &[u8] = b"station";
/// The station's authority at one of its venues: seeds `[VENUE_SEED, station,
/// venue program]`. It signs every call the station makes to that venue and
/// owns the station's accounts there, and signs nowhere else, so a venue's
/// code holds a signature that every other venue's accounts, and the
/// station's own dollar account, refuse.
pub const VENUE_SEED: &[u8] = b"venue";

/// The most drivers one station declares. A value, a close and a full
/// unwind ask every one inside one transaction, so a close through every
/// driver, each at the most markets its account may hold, must fit the
/// compute a transaction may ask for (1,400,000 units) and leave the caller
/// [`CALLER_COMPUTE`]. Each driver is held to
/// `shonin_driver::COMPUTE_PER_DRIVER`, and three of those fit; four would
/// not fit at all. Measured with the stand-in venue at eight markets each,
/// every close moving what it brings home through the venue's authority: a
/// vault's fractional close through three drivers takes about 1,040,000
/// units (1,066,000 with funding owed on every position), an unwinder's close
/// of a running station with funding owed about 1,072,000, and a stranger's
/// full unwind of a stopped station about 765,000 (792,000 with funding
/// owed). The station's record, with every driver's allowlist at its
/// largest, fits the room a program can give an account it creates (10 KiB).
pub const MAX_DRIVERS: usize = 3;
/// The most compute a transaction may ask for.
pub const MAX_COMPUTE: u64 = 1_400_000;
/// What a close through every driver leaves of [`MAX_COMPUTE`] for the
/// caller's own work around it in the same transaction: the vault's, serving
/// a claim.
pub const CALLER_COMPUTE: u64 = 200_000;
/// The most room a program can give an account it creates.
pub const MAX_CREATED: usize = 10_240;
/// The widest exit band a station can be created with, as the EVM station
/// factory caps it.
pub const MAX_EXIT_BAND_BPS: u16 = 1_000;
/// A stopped station's full unwind keeps the exit band this long after the
/// stop, then the band doubles each time this passes again.
pub const WIDEN_AFTER: u64 = 7 * DAY;

#[account]
#[derive(InitSpace)]
pub struct Config {
    /// The program whose vaults may create stations: its program-derived
    /// addresses, and only those, are vaults here. Fixed for good.
    pub vault_program: Pubkey,
    /// The dollar, under the classic token program. Fixed for good.
    pub mint: Pubkey,
    /// What stands for this cluster in the registry's blessings, copied from
    /// the registry, which fixes it for good.
    pub cluster: [u8; 32],
    pub bump: u8,
}

/// One driver the station declared, pinned for good when it was created: the
/// implementation the registry approved then, and the allowlist that
/// implementation gave for its venue.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct Pinned {
    /// The name `shonin.toml` writes, zero-padded.
    pub name: [u8; 32],
    /// The driver program.
    pub program: Pubkey,
    /// The one venue program the station may call through this driver.
    pub venue: Pubkey,
    /// The station's authority at this venue (seeds `[VENUE_SEED, station,
    /// venue]`): the only address the station signs a call to the venue
    /// with, the master of its account there, and the owner of every token
    /// account the venue's rules name as the station's.
    pub authority: Pubkey,
    pub authority_bump: u8,
    /// The authority's dollar account, its associated token account. The
    /// station moves dollars into it for a call that trades and takes back
    /// whatever is there after every call, so between calls it holds only
    /// what someone sent it unasked, which counts for nothing.
    pub dollars: Pubkey,
    /// The executor key approved to trade through it now; the default key
    /// while none is. One at a time. Its blessing is read again on every
    /// call it sends, so nothing here records when it runs out.
    pub key: Pubkey,
    /// The venue instructions the station may make, and the accounts in each
    /// that must be the station's own or one the allowlist pinned.
    pub rules: Vec<Rule>,
    /// The sets of the venue's accounts the rules pin, as the allowlist gave
    /// them.
    pub fixed: Vec<Vec<Pubkey>>,
    /// The venue's addresses for the station's authority there that the rules
    /// name, one per seed prefix: worked out once, when the station was
    /// created, since an address derived from the authority's never changes,
    /// so no call has to search for it again.
    pub addresses: Vec<StationAddress>,
}

/// The venue's program-derived address for the station's authority there,
/// with seeds `[prefix, authority]`.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct StationAddress {
    pub prefix: Vec<u8>,
    pub address: Pubkey,
}

impl Pinned {
    /// The venue's address for the station's authority under `prefix`, as
    /// pinned.
    pub fn address(&self, prefix: &[u8]) -> Option<&Pubkey> {
        self.addresses
            .iter()
            .find(|derived| derived.prefix == prefix)
            .map(|derived| &derived.address)
    }
}

/// A vault's station.
#[account]
pub struct Station {
    /// The vault: the program-derived address that signs for it.
    pub vault: Pubkey,
    /// The program the vault belongs to.
    pub vault_program: Pubkey,
    /// The vault's dollar account, where everything sent home goes.
    pub vault_dollars: Pubkey,
    /// The station's own dollar account: its associated token account. The
    /// vault's money arrives here and goes home from here, and no venue call
    /// is ever signed for it: it moves to and from a venue only by the
    /// station's own transfers to and from that venue's authority.
    pub dollars: Pubkey,
    pub mint: Pubkey,
    /// The bundle the vault runs: only a key blessed for it may trade here.
    pub bundle_hash: [u8; 32],
    /// The cluster the vault lives on, as the registry names it.
    pub cluster: [u8; 32],
    /// How far from the venue's own price a closing order may fill.
    pub exit_band_bps: u16,
    /// The dollars the station has accounted for in its own dollar account:
    /// what it was sent and what came back to it from its venues, less what
    /// it sent on. Dollars anyone sends the station's account, or a venue
    /// authority's, unasked are never counted.
    pub idle: u64,
    /// Every dollar the station has ever sent to the vault's dollar account,
    /// on every path: the vault's push and unwind, a key's unwind of a running
    /// station, and anyone's unwind of a stopped one. It only rises. The
    /// vault books what came home by how far this has risen since it last
    /// read it, whoever sent it, so a send it didn't make itself still
    /// counts, and a stranger's transfer to the vault never does.
    pub sent_home: u128,
    pub stopped: bool,
    /// When it stopped; zero while it runs.
    pub stopped_at: u64,
    /// Who paid for the station's accounts.
    pub payer: Pubkey,
    pub bump: u8,
    pub drivers: Vec<Pinned>,
}

impl Station {
    /// The index of the declared driver `name`.
    pub fn driver(&self, name: &[u8; 32]) -> Option<usize> {
        self.drivers.iter().position(|driver| &driver.name == name)
    }

    /// The band a stopped station's full unwind closes inside at `now`: the
    /// exit band for [`WIDEN_AFTER`] after the stop, then doubling each
    /// [`WIDEN_AFTER`] until any price is accepted. No key sets it; only time
    /// does.
    pub fn stopped_band(&self, now: u64) -> u16 {
        let band = self.exit_band_bps as u64;
        if !self.stopped || now < self.stopped_at.saturating_add(WIDEN_AFTER) {
            return band as u16;
        }
        let doublings = (now - self.stopped_at) / WIDEN_AFTER;
        // 2^14 basis points is past any price, so stop doubling there.
        if doublings >= 14 {
            return BPS as u16;
        }
        let band = band.max(1) << doublings;
        band.min(BPS) as u16
    }
}

/// What the station asks its vault before a stranger stops it or unwinds it
/// once stopped. The vault program answers `station_view`, given the vault's
/// account, with this in its return data.
pub mod vault {
    use anchor_lang::prelude::*;

    /// `sha256("global:station_view")[..8]`.
    pub const STATION_VIEW: [u8; 8] = [29, 130, 17, 121, 253, 235, 168, 218];

    #[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct VaultView {
        /// When the vault halted (a kill, a freeze, a retirement's end); zero
        /// while it runs.
        pub halted_at: u64,
        /// How long after a halt the executor has to wind down before anyone
        /// may stop the station.
        pub halt_grace: u64,
        /// Whether a priced valuation still has requests to settle: what a
        /// sale brings home then must not pay withdrawals priced before it.
        pub unsettled: bool,
    }
}

/// What a station answers `unwind` and `unwind_stopped` with.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub sent_home: u64,
    /// A market outside the band kept something where it is.
    pub blocked: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn station(band: u16, stopped_at: u64) -> Station {
        Station {
            vault: Pubkey::default(),
            vault_program: Pubkey::default(),
            vault_dollars: Pubkey::default(),
            dollars: Pubkey::default(),
            mint: Pubkey::default(),
            bundle_hash: [0; 32],
            cluster: [0; 32],
            exit_band_bps: band,
            idle: 0,
            sent_home: 0,
            stopped: stopped_at != 0,
            stopped_at,
            payer: Pubkey::default(),
            bump: 0,
            drivers: Vec::new(),
        }
    }

    #[test]
    fn a_close_through_every_driver_fits_a_transaction_with_room_to_spare() {
        let room = MAX_COMPUTE - CALLER_COMPUTE;
        assert!(MAX_DRIVERS as u64 * shonin_driver::COMPUTE_PER_DRIVER <= room);
        assert!((MAX_DRIVERS as u64 + 1) * shonin_driver::COMPUTE_PER_DRIVER > room);
    }

    #[test]
    fn the_largest_station_fits_the_room_a_program_can_give_it() {
        use shonin_driver::*;
        let seed = |i: usize| vec![i as u8; MAX_SEED];
        let rule = |i: u8| Rule {
            discriminator: vec![i; MAX_DISCRIMINATOR],
            class: Class::Trade,
            positions: (0..MAX_POSITIONS)
                .map(|p| Position {
                    index: p as u8,
                    own: Own::StationAddress(seed(p % MAX_STATION_ADDRESSES)),
                })
                .collect(),
        };
        let pinned = Pinned {
            name: [0; 32],
            program: Pubkey::default(),
            venue: Pubkey::default(),
            authority: Pubkey::default(),
            authority_bump: 0,
            dollars: Pubkey::default(),
            key: Pubkey::default(),
            rules: (0..MAX_RULES as u8).map(rule).collect(),
            // More than any answer could carry beside the largest rules.
            fixed: vec![vec![Pubkey::default(); MAX_FIXED]],
            addresses: (0..MAX_STATION_ADDRESSES)
                .map(|i| StationAddress {
                    prefix: seed(i),
                    address: Pubkey::default(),
                })
                .collect(),
        };
        let mut s = station(0, 0);
        s.drivers = vec![pinned; MAX_DRIVERS];
        let mut bytes = Vec::new();
        s.try_serialize(&mut bytes).unwrap();
        assert!(bytes.len() <= MAX_CREATED, "{} bytes", bytes.len());
    }

    #[test]
    fn the_stopped_band_holds_a_week_then_doubles_to_any_price() {
        let at = 1_000;
        let s = station(100, at);
        assert_eq!(s.stopped_band(at), 100);
        assert_eq!(s.stopped_band(at + WIDEN_AFTER - 1), 100);
        assert_eq!(s.stopped_band(at + WIDEN_AFTER), 200);
        assert_eq!(s.stopped_band(at + 2 * WIDEN_AFTER), 400);
        assert_eq!(s.stopped_band(at + 7 * WIDEN_AFTER), 10_000);
        assert_eq!(s.stopped_band(at + 100 * WIDEN_AFTER), 10_000);
        // A zero band still widens.
        assert_eq!(station(0, at).stopped_band(at + WIDEN_AFTER), 2);
        // A running station's band is the exit band.
        assert_eq!(station(100, 0).stopped_band(u64::MAX), 100);
    }
}
