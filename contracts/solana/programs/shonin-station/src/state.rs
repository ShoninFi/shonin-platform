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
    /// The authority's position mints at this venue, slot by slot, as many as
    /// the rules' largest [`shonin_driver::Own::StationMint`] names (none for
    /// a venue that holds no positions as tokens): worked out once, when the
    /// station was created, like `addresses`.
    pub position_mints: Vec<PositionMint>,
    /// The authority's associated token accounts, under the classic token
    /// program, for every mint of a set the rules' `StationToken` positions
    /// name: worked out once, when the station was created, like
    /// `addresses`, so that a call naming one is checked without working it
    /// out again.
    pub token_accounts: Vec<Pubkey>,
}

/// One of the authority's position mints: the station program's address
/// with seeds `[POSITION_MINT_SEED, authority, [slot]]`, its slot being its
/// place in [`Pinned::position_mints`].
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct PositionMint {
    pub address: Pubkey,
    pub bump: u8,
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

    /// Whether `key` is one of the authority's position mints at this venue.
    pub fn is_position_mint(&self, key: &Pubkey) -> bool {
        self.position_mints.iter().any(|mint| &mint.address == key)
    }
}

/// A vault's station.
#[account]
#[derive(Debug, PartialEq, Eq)]
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

/// Which layout a station's account is written in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// Today's: every driver ends with its position mints and its token
    /// accounts.
    Current,
    /// The first programs': every driver ends at its `addresses`
    /// ([`first`]). `migrate_station` rewrites such a station in the current
    /// layout.
    First,
}

impl Station {
    /// A station's account in either layout, and which: the account must
    /// carry the station's discriminator and its body must decode whole, with
    /// no byte left over, as the current layout, or failing that as the
    /// first. A station is made at exactly its size and its size never
    /// changes, in either layout (`migrate_station` resizes it to exactly the
    /// current layout's), so no account a program wrote has bytes past its
    /// record, zero or not. A station read in the first layout comes back
    /// with every driver's `position_mints` and `token_accounts` empty, as
    /// `migrate_station` writes it.
    ///
    /// Each layout is first walked over without allocating
    /// ([`walk::fills`]), every length checked against the bytes left, and
    /// decoded only if it fills the body exactly: decoding bytes in the wrong
    /// layout would otherwise read a length out of some other field and
    /// reserve room for it (a megabyte, for a list of bytes) before finding
    /// the bytes aren't there, which a program's heap can't hold.
    pub fn read_any(data: &[u8]) -> Option<(Station, Layout)> {
        let body = data.strip_prefix(Station::DISCRIMINATOR)?;
        if walk::fills(body, Layout::Current) {
            if let Some(station) = whole::<Station>(body) {
                return Some((station, Layout::Current));
            }
        }
        if !walk::fills(body, Layout::First) {
            return None;
        }
        whole::<first::Station>(body).map(|station| (station.into(), Layout::First))
    }
}

/// `T` decoded from all of `body`, or nothing if it doesn't decode or leaves
/// bytes over.
fn whole<T: AnchorDeserialize>(mut body: &[u8]) -> Option<T> {
    let value = T::deserialize(&mut body).ok()?;
    body.is_empty().then_some(value)
}

/// A station's body walked field by field in one layout, as Borsh lays it
/// out, without decoding or allocating anything: each list's length must fit
/// in the bytes left, each element taking at least one.
pub mod walk {
    use super::Layout;

    struct Cursor<'a>(&'a [u8]);

    impl Cursor<'_> {
        fn skip(&mut self, n: usize) -> Option<()> {
            self.0 = self.0.get(n..)?;
            Some(())
        }

        fn byte(&mut self) -> Option<u8> {
            let (first, rest) = self.0.split_first()?;
            self.0 = rest;
            Some(*first)
        }

        fn length(&mut self) -> Option<usize> {
            let bytes: [u8; 4] = self.0.get(..4)?.try_into().ok()?;
            self.0 = &self.0[4..];
            let n = u32::from_le_bytes(bytes) as usize;
            (n <= self.0.len()).then_some(n)
        }

        fn list(&mut self, each: impl Fn(&mut Self) -> Option<()>) -> Option<()> {
            for _ in 0..self.length()? {
                each(self)?;
            }
            Some(())
        }

        fn bytes(&mut self) -> Option<()> {
            let n = self.length()?;
            self.skip(n)
        }
    }

    /// `shonin_driver::Own`, every variant either layout may hold.
    fn own(c: &mut Cursor) -> Option<()> {
        match c.byte()? {
            // Station, StationDollars, StationPositionHeld.
            0 | 1 | 7 => Some(()),
            // StationToken(Option<u8>).
            2 => match c.byte()? {
                0 => Some(()),
                1 => c.skip(1),
                _ => None,
            },
            // StationAddress(Vec<u8>).
            3 => c.bytes(),
            // Fixed(u8), StationMint(u8), StationPositionToken(u8).
            4..=6 => c.skip(1),
            _ => None,
        }
    }

    fn rule(c: &mut Cursor) -> Option<()> {
        c.bytes()?;
        c.skip(1)?;
        c.list(|c| {
            c.skip(1)?;
            own(c)
        })
    }

    fn pinned(c: &mut Cursor, layout: Layout) -> Option<()> {
        // name, program, venue, authority, authority_bump, dollars, key.
        c.skip(32 * 6 + 1)?;
        c.list(rule)?;
        c.list(|c| c.list(|c| c.skip(32)))?;
        c.list(|c| {
            c.bytes()?;
            c.skip(32)
        })?;
        if layout == Layout::Current {
            c.list(|c| c.skip(33))?;
            c.list(|c| c.skip(32))?;
        }
        Some(())
    }

    /// Whether `body` (a station's account past its discriminator) walks
    /// through to its very end in `layout`.
    pub fn fills(body: &[u8], layout: Layout) -> bool {
        let mut c = Cursor(body);
        // vault, vault_program, vault_dollars, dollars, mint, bundle_hash,
        // cluster, exit_band_bps, idle, sent_home, stopped, stopped_at,
        // payer, bump.
        let walked = c
            .skip(32 * 7 + 2 + 8 + 16 + 1 + 8 + 32 + 1)
            .and_then(|()| c.list(|c| pinned(c, layout)));
        walked.is_some() && c.0.is_empty()
    }
}

/// The station's record as the first programs wrote it: the same fields in
/// the same order, every driver ending at its `addresses`. Their rules are
/// read with today's [`Rule`], whose [`shonin_driver::Own`] only gained
/// variants after the first ones, so the first rules decode unchanged.
pub mod first {
    use super::{Rule, StationAddress};
    use anchor_lang::prelude::*;

    #[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
    pub struct Pinned {
        pub name: [u8; 32],
        pub program: Pubkey,
        pub venue: Pubkey,
        pub authority: Pubkey,
        pub authority_bump: u8,
        pub dollars: Pubkey,
        pub key: Pubkey,
        pub rules: Vec<Rule>,
        pub fixed: Vec<Vec<Pubkey>>,
        pub addresses: Vec<StationAddress>,
    }

    #[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
    pub struct Station {
        pub vault: Pubkey,
        pub vault_program: Pubkey,
        pub vault_dollars: Pubkey,
        pub dollars: Pubkey,
        pub mint: Pubkey,
        pub bundle_hash: [u8; 32],
        pub cluster: [u8; 32],
        pub exit_band_bps: u16,
        pub idle: u64,
        pub sent_home: u128,
        pub stopped: bool,
        pub stopped_at: u64,
        pub payer: Pubkey,
        pub bump: u8,
        pub drivers: Vec<Pinned>,
    }

    impl From<Pinned> for super::Pinned {
        fn from(p: Pinned) -> Self {
            Self {
                name: p.name,
                program: p.program,
                venue: p.venue,
                authority: p.authority,
                authority_bump: p.authority_bump,
                dollars: p.dollars,
                key: p.key,
                rules: p.rules,
                fixed: p.fixed,
                addresses: p.addresses,
                // A first-layout station's rules hold no position mints. Its
                // token accounts are left unpinned: the guard derives an
                // unpinned station token account itself, as the first
                // layout's guard did, so the station is guarded as before.
                position_mints: Vec::new(),
                token_accounts: Vec::new(),
            }
        }
    }

    impl From<Station> for super::Station {
        fn from(s: Station) -> Self {
            Self {
                vault: s.vault,
                vault_program: s.vault_program,
                vault_dollars: s.vault_dollars,
                dollars: s.dollars,
                mint: s.mint,
                bundle_hash: s.bundle_hash,
                cluster: s.cluster,
                exit_band_bps: s.exit_band_bps,
                idle: s.idle,
                sent_home: s.sent_home,
                stopped: s.stopped,
                stopped_at: s.stopped_at,
                payer: s.payer,
                bump: s.bump,
                drivers: s.drivers.into_iter().map(Into::into).collect(),
            }
        }
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
            position_mints: vec![
                PositionMint {
                    address: Pubkey::default(),
                    bump: 0,
                };
                MAX_POSITION_MINTS
            ],
            // Every pinned account a mint, each with its token account.
            token_accounts: vec![Pubkey::default(); MAX_FIXED],
        };
        let mut s = station(0, 0);
        s.drivers = vec![pinned; MAX_DRIVERS];
        let mut bytes = Vec::new();
        s.try_serialize(&mut bytes).unwrap();
        assert!(bytes.len() <= MAX_CREATED, "{} bytes", bytes.len());
    }

    /// A driver as the first programs pinned it, its rules using every
    /// position kind they knew.
    fn first_pinned(seed: u8) -> first::Pinned {
        use shonin_driver::*;
        let key = |i: u8| Pubkey::new_from_array([seed.wrapping_add(i); 32]);
        first::Pinned {
            name: [b'a' + seed; 32],
            program: key(1),
            venue: key(2),
            authority: key(3),
            authority_bump: seed,
            dollars: key(4),
            key: key(5),
            rules: vec![
                Rule {
                    discriminator: vec![seed, 1, 2],
                    class: Class::Trade,
                    positions: vec![
                        Position {
                            index: 0,
                            own: Own::Station,
                        },
                        Position {
                            index: 1,
                            own: Own::StationDollars,
                        },
                        Position {
                            index: 2,
                            own: Own::StationToken(None),
                        },
                        Position {
                            index: 3,
                            own: Own::StationToken(Some(0)),
                        },
                    ],
                },
                Rule {
                    discriminator: vec![seed, 9],
                    class: Class::Exit,
                    positions: vec![
                        Position {
                            index: 0,
                            own: Own::StationAddress(b"margin".to_vec()),
                        },
                        Position {
                            index: 1,
                            own: Own::Fixed(0),
                        },
                    ],
                },
            ],
            fixed: vec![vec![key(6), key(7)]],
            addresses: vec![StationAddress {
                prefix: b"margin".to_vec(),
                address: key(8),
            }],
        }
    }

    fn first_station(drivers: usize) -> first::Station {
        first::Station {
            vault: Pubkey::new_from_array([11; 32]),
            vault_program: Pubkey::new_from_array([12; 32]),
            vault_dollars: Pubkey::new_from_array([13; 32]),
            dollars: Pubkey::new_from_array([14; 32]),
            mint: Pubkey::new_from_array([15; 32]),
            bundle_hash: [16; 32],
            cluster: [17; 32],
            exit_band_bps: 0x1819,
            idle: 0x2021_2223_2425_2627,
            sent_home: 0x3031_3233_3435_3637_3839_3a3b_3c3d_3e3f,
            stopped: true,
            stopped_at: 0x4041_4243,
            payer: Pubkey::new_from_array([18; 32]),
            bump: 254,
            drivers: (0..drivers as u8).map(first_pinned).collect(),
        }
    }

    fn account(body: &impl AnchorSerialize) -> Vec<u8> {
        let mut data = Station::DISCRIMINATOR.to_vec();
        body.serialize(&mut data).unwrap();
        data
    }

    #[test]
    fn a_first_layout_station_reads_whole_with_its_new_lists_empty() {
        for drivers in 1..=MAX_DRIVERS {
            let old = first_station(drivers);
            let (read, layout) = Station::read_any(&account(&old)).unwrap();
            assert_eq!(layout, Layout::First);
            assert_eq!(read, Station::from(old.clone()));
            assert_eq!(read.drivers.len(), drivers);
            assert!(read
                .drivers
                .iter()
                .all(|d| d.position_mints.is_empty() && d.token_accounts.is_empty()));
            // Every value carried over: written back in the first layout it
            // is the same bytes.
            let back = first::Station {
                drivers: read
                    .drivers
                    .iter()
                    .map(|d| first::Pinned {
                        name: d.name,
                        program: d.program,
                        venue: d.venue,
                        authority: d.authority,
                        authority_bump: d.authority_bump,
                        dollars: d.dollars,
                        key: d.key,
                        rules: d.rules.clone(),
                        fixed: d.fixed.clone(),
                        addresses: d.addresses.clone(),
                    })
                    .collect(),
                ..old.clone()
            };
            assert_eq!(account(&back), account(&old));
        }
    }

    #[test]
    fn a_current_station_reads_as_the_current_layout() {
        for drivers in 0..=MAX_DRIVERS {
            let mut current = Station::from(first_station(drivers));
            for (i, driver) in current.drivers.iter_mut().enumerate() {
                driver.position_mints = vec![
                    PositionMint {
                        address: Pubkey::new_from_array([90 + i as u8; 32]),
                        bump: 7,
                    };
                    i
                ];
                driver.token_accounts = vec![Pubkey::new_from_array([80; 32]); i + 1];
            }
            let mut data = Vec::new();
            current.try_serialize(&mut data).unwrap();
            assert_eq!(
                Station::read_any(&data),
                Some((current.clone(), Layout::Current))
            );
        }
        // Empty new lists too: what a migrated station holds.
        let migrated = Station::from(first_station(2));
        let mut data = Vec::new();
        migrated.try_serialize(&mut data).unwrap();
        assert_eq!(Station::read_any(&data), Some((migrated, Layout::Current)));
    }

    #[test]
    fn a_station_with_bytes_past_its_record_or_cut_short_is_not_read() {
        for drivers in 1..=MAX_DRIVERS {
            let old = account(&first_station(drivers));
            let mut current = Vec::new();
            Station::from(first_station(drivers))
                .try_serialize(&mut current)
                .unwrap();
            for data in [old, current] {
                for extra in [0u8, 1, 0xff] {
                    let mut longer = data.clone();
                    longer.push(extra);
                    assert_eq!(Station::read_any(&longer), None, "{extra} past the end");
                }
                for cut in [1, 5, data.len() - 9] {
                    assert_eq!(Station::read_any(&data[..data.len() - cut]), None);
                }
                let mut other = data.clone();
                other[0] ^= 1;
                assert_eq!(Station::read_any(&other), None, "another discriminator");
            }
        }
    }

    fn current_bytes(station: &Station) -> Vec<u8> {
        let mut data = Vec::new();
        station.try_serialize(&mut data).unwrap();
        data
    }

    /// Every body either layout decodes whole is one the walk goes through,
    /// so the walk never turns a readable station away; checked over the
    /// stations above and every one-byte change to each.
    #[test]
    fn whatever_decodes_whole_in_a_layout_walks_through_in_it() {
        let mut bodies = Vec::new();
        for drivers in 0..=MAX_DRIVERS {
            let old = first_station(drivers);
            bodies.push(account(&old)[8..].to_vec());
            let mut current = Station::from(old);
            for driver in current.drivers.iter_mut() {
                driver.position_mints = vec![
                    PositionMint {
                        address: Pubkey::new_from_array([3; 32]),
                        bump: 1,
                    };
                    2
                ];
                driver.token_accounts = vec![Pubkey::new_from_array([4; 32])];
            }
            bodies.push(current_bytes(&current)[8..].to_vec());
        }
        let mut checked = 0;
        for body in bodies {
            for at in 0..body.len() {
                for value in [0u8, 1, 2, 3, 7, 8, 33, 0xff] {
                    let mut changed = body.clone();
                    changed[at] = value;
                    if whole::<Station>(&changed).is_some() {
                        assert!(walk::fills(&changed, Layout::Current), "at {at}");
                        checked += 1;
                    }
                    if whole::<first::Station>(&changed).is_some() {
                        assert!(walk::fills(&changed, Layout::First), "at {at}");
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked > 1_000, "{checked}");
    }

    /// A first-layout station whose second driver's name is one character
    /// starts, read in the current layout, with a short list's length: the
    /// walk refuses it there without reserving anything, and it reads in the
    /// first layout.
    #[test]
    fn a_first_layout_station_with_a_one_letter_driver_reads_in_the_first_layout() {
        for drivers in 2..=MAX_DRIVERS {
            let mut old = first_station(drivers);
            for (i, driver) in old.drivers.iter_mut().enumerate() {
                driver.name = [0; 32];
                driver.name[0] = b'a' + i as u8;
            }
            let data = account(&old);
            assert!(!walk::fills(&data[8..], Layout::Current));
            let (read, layout) = Station::read_any(&data).unwrap();
            assert_eq!(layout, Layout::First);
            assert_eq!(read, Station::from(old));
        }
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
