//! The bookkeeping every part of the vault shares, as the EVM vault's
//! `VaultStorage`: moving shares, paying out (or holding a payout a receiver
//! can't take), booking what the station sends home, and the questions every
//! rule asks (is a valuation owed its settlement, is the vault ready to
//! close, what is dust).

use anchor_lang::prelude::*;
use anchor_spl::associated_token::get_associated_token_address_with_program_id;
use anchor_spl::token::{self, TransferChecked};
use shonin_common::math::U256;
use shonin_driver::MAX_MARKETS;

use crate::error::VaultError;
use crate::events::*;
use crate::maths::{mul_div, mul_div_up, store_wide, wide};
use crate::state::*;

// Where it is in its life ---------------------------------------------------------

impl Vault {
    /// Taking deposits or winding down: the strategy runs.
    pub fn live(&self) -> bool {
        matches!(self.state, VaultState::Open | VaultState::WindingDown)
    }

    pub fn halted(&self) -> bool {
        self.halted_at != 0
    }

    /// Whether the last priced valuation still has requests waiting on it. A
    /// new valuation waits until they are settled, so each prices only its
    /// own; a closed vault settles what is left through its own doors.
    pub fn owed(&self) -> bool {
        self.priced && self.state != VaultState::Closed && self.request_cursor < self.priced_through
    }

    /// Halted, and a valuation since has found the station empty: nothing is
    /// left to price, and the vault only waits to close. No new request is
    /// taken, so nobody can hold the close off a slot at a time.
    pub fn ready_to_close(&self) -> bool {
        self.halted() && self.priced && self.latest.time > self.halted_at && self.empty()
    }

    /// The station held nothing at the last priced valuation beyond dust
    /// nobody can be made to own (a tenth of the minimum deposit), judged on
    /// its higher figure, so a stranger's micro-dollar can't hold a halted
    /// vault's close off.
    pub fn empty(&self) -> bool {
        self.latest.gross.saturating_add(self.latest.extra)
            <= self.latest.cash as u128 + 1 + (self.minimum_deposit / 10) as u128
    }

    /// What a close or a serve may leave behind and still have served nobody
    /// short beyond rounding. The station rounds each order's proceeds down at
    /// its venue, leaves alone a venue account worth no more than one more
    /// dollar-unit than the orders its close would place, and closes a
    /// leaver's part of one worth up to the square of that without refusing
    /// it for paying nobody. With every venue holding up to `MAX_MARKETS`
    /// markets, a leaver's part of `d` venues can come home as nothing while
    /// worth up to `d * (MAX_MARKETS + 1)^2` at the mark, and one more for
    /// the station's idle dollars.
    pub fn dust(&self) -> u128 {
        let per_venue = (MAX_MARKETS as u128 + 1) * (MAX_MARKETS as u128 + 1);
        self.drivers.len() as u128 * per_venue + 1
    }

    /// How many requests a valuation at `time` prices: every one whose
    /// `earliest` is at or before it. Requests are made in order of
    /// `earliest`; while one is being recorded no request can be later than
    /// two slots on (the lead and the window are each shorter than a slot),
    /// so at most the last two groups are later than `time`.
    pub fn priced_by(&self, time: u64) -> u64 {
        if self.request_count == 0 || self.tail_earliest <= time {
            return self.request_count;
        }
        if self.tail_start == 0 || self.previous_earliest <= time {
            return self.tail_start;
        }
        self.previous_start
    }

    /// Records a new request's place among the groups `priced_by` reads.
    pub fn note_request(&mut self, earliest: u64) {
        let index = self.request_count;
        if index == 0 || earliest != self.tail_earliest {
            self.previous_earliest = self.tail_earliest;
            self.previous_start = self.tail_start;
            self.tail_earliest = earliest;
            self.tail_start = index;
        }
        self.request_count = index + 1;
    }

    /// The idle floor in force: a lowering applies once its delay has passed.
    pub fn floor_now(&self, now: u64) -> u16 {
        if self.pending_floor_at != 0 && now >= self.pending_floor_at {
            return self.pending_floor_bps;
        }
        self.floor_bps
    }

    // Units -------------------------------------------------------------------------

    pub fn scale(&self) -> u128 {
        if self.unit_scale == 0 {
            WAD
        } else {
            self.unit_scale
        }
    }

    /// The fraction of the station, scaled by `WAD`, the unit holders own.
    pub fn live_units(&self) -> Result<u128> {
        if self.units == 0 {
            return Ok(0);
        }
        mul_div(self.units, self.scale(), WAD)
    }

    // Shares ------------------------------------------------------------------------

    pub fn mint_to(&mut self, holder: &mut Holder, amount: u128, said: &mut Said) -> Result<()> {
        if amount == 0 {
            return Ok(());
        }
        self.total_supply = self
            .total_supply
            .checked_add(amount)
            .ok_or(VaultError::Overflow)?;
        holder.shares = holder
            .shares
            .checked_add(amount)
            .ok_or(VaultError::Overflow)?;
        said.say(Transfer {
            from: Pubkey::default(),
            to: holder.owner,
            value: amount,
        });
        Ok(())
    }

    /// Burns shares the vault holds (pending ones, already counted out of
    /// `pending_shares` by the caller).
    pub fn burn_held(&mut self, amount: u128, vault: &Pubkey, said: &mut Said) -> Result<()> {
        if amount == 0 {
            return Ok(());
        }
        self.total_supply = self
            .total_supply
            .checked_sub(amount)
            .ok_or(VaultError::Overflow)?;
        said.say(Transfer {
            from: *vault,
            to: Pubkey::default(),
            value: amount,
        });
        Ok(())
    }

    pub fn burn_from(&mut self, holder: &mut Holder, amount: u128, said: &mut Said) -> Result<()> {
        if amount == 0 {
            return Ok(());
        }
        holder.shares = holder
            .shares
            .checked_sub(amount)
            .ok_or(VaultError::TooSmall)?;
        self.total_supply = self
            .total_supply
            .checked_sub(amount)
            .ok_or(VaultError::Overflow)?;
        said.say(Transfer {
            from: holder.owner,
            to: Pubkey::default(),
            value: amount,
        });
        Ok(())
    }

    /// Hands a fee slice of shares the vault holds to the author and the
    /// platform, split by their parts of the profit fee.
    pub fn hand_slice(
        &mut self,
        slice: u128,
        author: &mut Holder,
        platform: &mut Holder,
        vault: &Pubkey,
        said: &mut Said,
    ) -> Result<()> {
        if slice == 0 {
            return Ok(());
        }
        let total = self.fees.author_bps as u128 + self.fees.platform_bps as u128;
        let to_author = mul_div(slice, self.fees.author_bps as u128, total)?;
        author.shares = author
            .shares
            .checked_add(to_author)
            .ok_or(VaultError::Overflow)?;
        platform.shares = platform
            .shares
            .checked_add(slice - to_author)
            .ok_or(VaultError::Overflow)?;
        said.say(Transfer {
            from: *vault,
            to: author.owner,
            value: to_author,
        });
        said.say(Transfer {
            from: *vault,
            to: platform.owner,
            value: slice - to_author,
        });
        Ok(())
    }

    // The station's sends home -------------------------------------------------------

    /// Books `amount` the station sent home as the pool's: the unit holders'
    /// part to their reserve, the rest to cash.
    pub fn from_station(&mut self, amount: u64, said: &mut Said) -> Result<()> {
        if amount == 0 {
            return Ok(());
        }
        let mut to_units: u64 = 0;
        if self.units != 0 {
            to_units = mul_div(amount as u128, self.live_units()?, WAD)? as u64;
            let per_unit = wide(&self.per_unit)
                + U256::from(to_units) * U256::from(UNIT_SCALE) / U256::from(self.units);
            self.per_unit = store_wide(per_unit);
            self.unit_reserve = self
                .unit_reserve
                .checked_add(to_units)
                .ok_or(VaultError::Overflow)?;
        }
        self.cash = self
            .cash
            .checked_add(amount - to_units)
            .ok_or(VaultError::Overflow)?;
        said.say(FromStation {
            assets: amount,
            to_units,
        });
        Ok(())
    }

    /// What the station has sent home since the vault last looked, whoever
    /// made it send: the rise in its running total. Only the station program
    /// can write that total, so nothing a stranger sends the vault's dollar
    /// account directly is ever counted.
    pub fn arrived(&mut self, station: &AccountInfo) -> Result<u64> {
        let total = station_sent_home(station, &self.station)?;
        let arrived = total
            .checked_sub(self.station_booked)
            .ok_or(VaultError::WrongAccount)?;
        self.station_booked = total;
        u64::try_from(arrived).map_err(|_| error!(VaultError::Overflow))
    }

    /// Books whatever the station has sent home since the vault last looked
    /// as the pool's.
    pub fn sync_arrivals(&mut self, station: &AccountInfo, said: &mut Said) -> Result<()> {
        let arrived = self.arrived(station)?;
        self.from_station(arrived, said)
    }
}

/// Where a station's running total of what it sent home sits in its account:
/// after the discriminator, five keys, the bundle and the cluster, the exit
/// band and the counted dollars.
pub const SENT_HOME_AT: usize = 8 + 32 * 5 + 32 + 32 + 2 + 8;

/// The station's running total of what it has sent home, read from its
/// account, which must be the vault's station.
pub fn station_sent_home(station: &AccountInfo, expected: &Pubkey) -> Result<u128> {
    use anchor_lang::Discriminator;
    require_keys_eq!(*station.key, *expected, VaultError::WrongAccount);
    require_keys_eq!(*station.owner, shonin_station::ID, VaultError::WrongAccount);
    let data = station.try_borrow_data()?;
    require!(
        data.len() >= SENT_HOME_AT + 16
            && data[..8] == *shonin_station::state::Station::DISCRIMINATOR,
        VaultError::WrongAccount
    );
    let bytes: [u8; 16] = data[SENT_HOME_AT..SENT_HOME_AT + 16]
        .try_into()
        .map_err(|_| error!(VaultError::WrongAccount))?;
    Ok(u128::from_le_bytes(bytes))
}

// The holder an instruction acts on ------------------------------------------------

/// A holding an instruction acts on beside the pool's own copies of the
/// author's and the platform's. When it is one of those, the change is made
/// to the pool's copy, which is what is written back; two copies of one
/// account written back in turn would lose whichever went first.
pub enum Subject {
    Author,
    Platform,
    /// Anyone else's, as it is in its account; `None` if it has none yet.
    Other(Option<Box<Holder>>),
}

/// Which holding `info`, `owner`'s in this vault, is.
pub fn subject_of(
    info: &AccountInfo,
    vault_key: &Pubkey,
    owner: &Pubkey,
    author: &Holder,
    platform: &Holder,
) -> Result<Subject> {
    let address = holder_address(vault_key, owner).0;
    require_keys_eq!(*info.key, address, VaultError::WrongAccount);
    if *owner == author.owner {
        return Ok(Subject::Author);
    }
    if *owner == platform.owner {
        return Ok(Subject::Platform);
    }
    Ok(Subject::Other(
        load::<Holder>(info, &address)?.map(Box::new),
    ))
}

impl Subject {
    /// A working copy of the holding.
    pub fn copy(
        &self,
        vault_key: &Pubkey,
        owner: &Pubkey,
        author: &Holder,
        platform: &Holder,
    ) -> Holder {
        match self {
            Subject::Author => author.clone(),
            Subject::Platform => platform.clone(),
            Subject::Other(Some(holder)) => (**holder).clone(),
            Subject::Other(None) => Holder {
                vault: *vault_key,
                owner: *owner,
                shares: 0,
                statement: [0; 32],
                units: 0,
                unit_debt: 0,
                unit_credit: 0,
                units_since: 0,
                pending: 0,
                payer: *owner,
                bump: holder_address(vault_key, owner).1,
            },
        }
    }

    /// Writes the working copy back: the units and the pending deposits into
    /// the pool's copy for the author or the platform (the only parts of a
    /// holding these instructions change there), or the whole holding into
    /// its own account, created if it has none, `payer` paying.
    pub fn put_back<'info>(
        self,
        copy: &Holder,
        info: &AccountInfo<'info>,
        author: &mut Holder,
        platform: &mut Holder,
        payer: Option<(&AccountInfo<'info>, &AccountInfo<'info>)>,
    ) -> Result<()> {
        let absorb = |target: &mut Holder| {
            target.units = copy.units;
            target.unit_debt = copy.unit_debt;
            target.unit_credit = copy.unit_credit;
            target.units_since = copy.units_since;
            target.pending = copy.pending;
        };
        match self {
            Subject::Author => absorb(author),
            Subject::Platform => absorb(platform),
            Subject::Other(Some(_)) => store(info, copy)?,
            Subject::Other(None) => {
                let (payer, system_program) = payer.ok_or(VaultError::WrongAccount)?;
                shonin_common::account::create_pda(
                    payer,
                    info,
                    system_program,
                    &[
                        HOLDER_SEED,
                        copy.vault.as_ref(),
                        copy.owner.as_ref(),
                        &[copy.bump],
                    ],
                    8 + Holder::INIT_SPACE,
                    &crate::ID,
                )?;
                store(info, copy)?;
            }
        }
        Ok(())
    }
}

// Units ------------------------------------------------------------------------------

/// What a holder's units have earned since they last looked: measured down,
/// from a starting point measured up, so no holder is ever credited more than
/// came home for them, whatever the rounding of the others.
pub fn accrue(vault: &Vault, holder: &mut Holder) -> Result<()> {
    let per_unit = wide(&vault.per_unit);
    let product = U256::from(holder.units) * per_unit;
    let scale = U256::from(UNIT_SCALE);
    let earned = product / scale;
    let mut debt = earned;
    if !(product % scale).is_zero() {
        debt += U256::one();
    }
    let earned = narrow(earned)?;
    if earned > holder.unit_debt {
        holder.unit_credit = holder
            .unit_credit
            .checked_add(
                u64::try_from(earned - holder.unit_debt)
                    .map_err(|_| error!(VaultError::Overflow))?,
            )
            .ok_or(VaultError::Overflow)?;
    }
    holder.unit_debt = narrow(debt)?;
    Ok(())
}

fn narrow(value: U256) -> Result<u128> {
    if value > U256::from(u128::MAX) {
        return err!(VaultError::Overflow);
    }
    Ok(value.as_u128())
}

/// The units' debt for a holder's whole balance at the current accumulator,
/// rounded up.
pub fn unit_debt_of(vault: &Vault, units: u128) -> Result<u128> {
    let product = U256::from(units) * wide(&vault.per_unit);
    let scale = U256::from(UNIT_SCALE);
    let mut debt = product / scale;
    if !(product % scale).is_zero() {
        debt += U256::one();
    }
    narrow(debt)
}

// Paying out -----------------------------------------------------------------------------

/// The vault's dollar account and what a transfer out of it needs.
pub struct Purse<'a, 'info> {
    pub vault: AccountInfo<'info>,
    pub dollars: AccountInfo<'info>,
    pub mint: AccountInfo<'info>,
    pub token_program: AccountInfo<'info>,
    pub decimals: u8,
    pub seeds: &'a [&'a [u8]],
}

/// The classic token program's account layout: the mint, the owner, the
/// amount, then the state (1 is initialised, 2 frozen) at this offset.
const TOKEN_ACCOUNT_LEN: usize = 165;
const STATE_AT: usize = 108;
const INITIALIZED: u8 = 1;

impl<'info> Purse<'_, 'info> {
    /// Sends `amount` of the vault's dollars to `to`, signed by the vault.
    pub fn send(&self, amount: u64, to: &AccountInfo<'info>) -> Result<()> {
        if amount == 0 {
            return Ok(());
        }
        token::transfer_checked(
            CpiContext::new_with_signer(
                self.token_program.clone(),
                TransferChecked {
                    from: self.dollars.clone(),
                    mint: self.mint.clone(),
                    to: to.clone(),
                    authority: self.vault.clone(),
                },
                &[self.seeds],
            ),
            amount,
            self.decimals,
        )
    }

    /// Whether `to` is `owner`'s associated dollar account and can take a
    /// payout now: it exists, holds the dollar, is `owner`'s, and is not
    /// frozen. Any other account in its place is an error, not a refusal, so
    /// whoever sends the transaction can't choose to have a payout held.
    pub fn takes(&self, to: &AccountInfo, owner: &Pubkey) -> Result<bool> {
        let expected = get_associated_token_address_with_program_id(
            owner,
            self.mint.key,
            self.token_program.key,
        );
        require_keys_eq!(*to.key, expected, VaultError::WrongAccount);
        if to.owner != self.token_program.key {
            return Ok(false);
        }
        let data = to.try_borrow_data()?;
        Ok(data.len() == TOKEN_ACCOUNT_LEN
            && data[..32] == self.mint.key.to_bytes()
            && data[32..64] == owner.to_bytes()
            && data[STATE_AT] == INITIALIZED)
    }

    /// Pays `amount` to `owner`'s associated dollar account `to`, if it can
    /// take it; answers whether it was paid. One the account can't take is
    /// for the caller to hold, so one receiver never stops the queue.
    pub fn pay(
        &self,
        to: &AccountInfo<'info>,
        owner: &Pubkey,
        amount: u64,
        said: &mut Said,
    ) -> Result<bool> {
        if amount == 0 {
            return Ok(true);
        }
        if !self.takes(to, owner)? {
            said.say(Held {
                to: *owner,
                assets: amount,
            });
            return Ok(false);
        }
        self.send(amount, to)?;
        Ok(true)
    }
}

/// A token account the signer named for their own money: any account of the
/// dollar's, which the token program checks as it transfers.
pub fn own_account(to: &AccountInfo, owner: &Pubkey, mint: &Pubkey) -> Result<()> {
    require!(to.owner == &anchor_spl::token::ID, VaultError::WrongAccount);
    let data = to.try_borrow_data()?;
    require!(
        data.len() == TOKEN_ACCOUNT_LEN
            && data[..32] == mint.to_bytes()
            && data[32..64] == owner.to_bytes(),
        VaultError::WrongAccount
    );
    Ok(())
}

// Accounts this program creates and closes ------------------------------------------------

/// Closes `account`, which this program owns, sending its rent to `to`: its
/// lamports go, its owner becomes the system program and its data is
/// emptied, so nothing in the same transaction can revive it as it was.
pub fn close_account<'info>(account: &AccountInfo<'info>, to: &AccountInfo<'info>) -> Result<()> {
    let lamports = account.lamports();
    **to.try_borrow_mut_lamports()? = to
        .lamports()
        .checked_add(lamports)
        .ok_or(VaultError::Overflow)?;
    **account.try_borrow_mut_lamports()? = 0;
    account.assign(&anchor_lang::system_program::ID);
    account.resize(0)?;
    Ok(())
}

/// Loads an account of type `T` this program owns at `expected`; `None` if
/// nothing (or nothing of this program's) is there, as for a request already
/// closed.
pub fn load<T: AccountDeserialize>(account: &AccountInfo, expected: &Pubkey) -> Result<Option<T>> {
    require_keys_eq!(*account.key, *expected, VaultError::WrongAccount);
    if account.owner != &crate::ID || account.data_is_empty() {
        return Ok(None);
    }
    let data = account.try_borrow_data()?;
    Ok(Some(T::try_deserialize(&mut &data[..])?))
}

/// Loads the vault's request `id` from `account`, wherever it is: a
/// request's address is its maker's, so it is known by what it holds. `None`
/// if nothing (or nothing of this program's) is there, as for a request
/// already closed; anything else of this program's is refused.
pub fn load_request(account: &AccountInfo, vault: &Pubkey, id: u64) -> Result<Option<Request>> {
    use anchor_lang::Discriminator;
    if account.owner != &crate::ID || account.data_is_empty() {
        return Ok(None);
    }
    let data = account.try_borrow_data()?;
    require!(
        data.starts_with(Request::DISCRIMINATOR),
        VaultError::WrongAccount
    );
    let r = Request::try_deserialize(&mut &data[..])?;
    require!(r.vault == *vault && r.id == id, VaultError::WrongAccount);
    Ok(Some(r))
}

/// Whether `account` holds a claim of this program's, by its discriminator
/// alone: where it is and whose it is are for `load_claim` to check.
pub fn holds_claim(account: &AccountInfo) -> Result<bool> {
    use anchor_lang::Discriminator;
    if account.owner != &crate::ID || account.data_is_empty() {
        return Ok(false);
    }
    Ok(account.try_borrow_data()?.starts_with(Claim::DISCRIMINATOR))
}

/// Loads the vault's claim `id` from `account`, which must be at its address
/// (the vault and the id are its seeds) and hold that id. `None` if nothing
/// (or nothing of this program's) is there, as for a claim already closed;
/// anything else of this program's is refused.
pub fn load_claim(account: &AccountInfo, vault: &Pubkey, id: u64) -> Result<Option<Claim>> {
    require_keys_eq!(
        *account.key,
        claim_address(vault, id).0,
        VaultError::WrongAccount
    );
    if account.owner != &crate::ID || account.data_is_empty() {
        return Ok(None);
    }
    require!(holds_claim(account)?, VaultError::WrongAccount);
    let data = account.try_borrow_data()?;
    let claim = Claim::try_deserialize(&mut &data[..])?;
    require!(claim.id == id, VaultError::WrongAccount);
    Ok(Some(claim))
}

/// Writes `value` back into `account`.
pub fn store<T: AccountSerialize>(account: &AccountInfo, value: &T) -> Result<()> {
    let mut data = account.try_borrow_mut_data()?;
    let mut writer: &mut [u8] = &mut data;
    value.try_serialize(&mut writer)?;
    Ok(())
}

/// The rent-paid unit a request or claim account needs.
pub fn rent_for(space: usize) -> Result<u64> {
    Ok(Rent::get()?.minimum_balance(space))
}

/// Rounds a holder's units' debt for its balance now, as the units change.
pub fn reset_debt(vault: &Vault, holder: &mut Holder) -> Result<()> {
    holder.unit_debt = unit_debt_of(vault, holder.units)?;
    Ok(())
}

/// `mul_div_up` re-exported for the parts that compute a ceiling slice.
pub fn ceil(a: u128, b: u128, d: u128) -> Result<u128> {
    mul_div_up(a, b, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::maths::next_slot;

    /// A vault with every field zero but its timers.
    fn vault(timers: Timers) -> Vault {
        let zeroes = vec![0u8; 8 + Vault::INIT_SPACE];
        let mut vault = Vault::try_deserialize_unchecked(&mut &zeroes[..]).unwrap();
        vault.timers = timers;
        vault
    }

    #[test]
    fn the_station_total_is_read_where_the_station_writes_it() {
        use anchor_lang::AccountSerialize;
        let station = shonin_station::state::Station {
            vault: Pubkey::new_unique(),
            vault_program: Pubkey::new_unique(),
            vault_dollars: Pubkey::new_unique(),
            dollars: Pubkey::new_unique(),
            mint: Pubkey::new_unique(),
            bundle_hash: [3; 32],
            cluster: [4; 32],
            exit_band_bps: 100,
            idle: 77,
            sent_home: 0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10,
            stopped: true,
            stopped_at: 9,
            payer: Pubkey::new_unique(),
            bump: 1,
            drivers: Vec::new(),
        };
        let mut data = Vec::new();
        station.try_serialize(&mut data).unwrap();
        let read = u128::from_le_bytes(data[SENT_HOME_AT..SENT_HOME_AT + 16].try_into().unwrap());
        assert_eq!(read, station.sent_home);
    }

    /// Requests made at random moments, valuations recorded at random moments
    /// inside their windows: `priced_by` always counts exactly the requests
    /// whose `earliest` is at or before the valuation's time.
    #[test]
    fn a_valuation_prices_exactly_the_requests_due_by_its_time() {
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        for (interval, lead, window) in [(10, 5, 3), (10, 9, 8), (10, 4, 3), (7, 3, 2), (60, 30, 5)]
        {
            let timers = Timers {
                slot_interval: interval,
                slot_lead: lead,
                slot_window: window,
                ..Timers::default()
            };
            for _ in 0..200 {
                let mut v = vault(timers);
                let mut earliest_of = Vec::new();
                let mut now = 1_000_000u64;
                for _ in 0..60 {
                    now += next(3 * interval);
                    if next(3) == 0 {
                        // A valuation at the slot boundary just passed, if
                        // `now` is inside its window.
                        let time = now - now % interval;
                        if now - time <= window {
                            let expected =
                                earliest_of.iter().filter(|e| **e <= time).count() as u64;
                            assert_eq!(v.priced_by(time), expected);
                        }
                    } else {
                        let earliest = next_slot(&timers, now).unwrap();
                        earliest_of.push(earliest);
                        v.note_request(earliest);
                    }
                }
            }
        }
    }

    #[test]
    fn dust_grows_with_every_venue_a_station_pins() {
        let mut v = vault(Timers::default());
        assert_eq!(v.dust(), 1);
        v.drivers = vec![[0; 32]];
        assert_eq!(v.dust(), 82);
        v.drivers = vec![[0; 32]; 3];
        assert_eq!(v.dust(), 244);
    }
}
