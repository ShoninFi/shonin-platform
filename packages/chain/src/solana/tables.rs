//! Address lookup tables: how a transaction that names more accounts than
//! the wire carries by address is sent.
//!
//! A transaction goes as a legacy one, every account named by address, when
//! that fits the wire's 1,232 bytes. One that doesn't (a station with three
//! drivers activated, a close through three venues, a valuation at the
//! market limits, an executor's call naming many markets or orders) goes as
//! a version-0 transaction that finds its accounts through one lookup table,
//! a byte each instead of thirty-two. Its simulation is compiled the same
//! way, through the same table, so a refusal there predicts the send.
//!
//! - **Who pays and owns.** The key paying for the transaction (its first
//!   signer: the keeper, a vault's executor key, an author activating, the
//!   deployer) pays for its own tables and is their authority. No key relies
//!   on a table another key could change, and no role pays for another's.
//!   For the executor that rent is part of what its key spends on its own
//!   sends, and is billed as gas like any other rent it puts into accounts:
//!   [`Landed`](super::client::Landed) counts a send's table set-up with it.
//!   The keeper pays for its tables as for any of its sends.
//! - **What goes in.** Every account the transaction names except its
//!   signers (the payer among them) and the programs it calls, which a
//!   version-0 message must name by address. Writable accounts go in too:
//!   whether an account is written is said by the message, not the table,
//!   and is kept. The accounts these transactions name are the programs'
//!   addresses (the vault, its station, the registry's and the venues'
//!   records, mints, markets, sysvars), the same from one call to the next.
//! - **Reuse.** A key's tables are one address book, grown as new addresses
//!   appear: a transaction uses the table already holding everything it
//!   names; otherwise the table holding most of it is extended with the
//!   rest while there is room (256 addresses), and otherwise a new table is
//!   made holding all of it. One table per transaction keeps it small. A
//!   table's address is derived from its authority and a recent slot, so it
//!   can't be worked out again: a key's tables are found on the cluster (by
//!   their authority) the first time the key needs one, and remembered from
//!   then on, so they are reused across restarts.
//! - **Checked before use.** A table is read from the cluster before every
//!   use, and used only if the lookup-table program owns it, its authority
//!   is the paying key, it isn't deactivated, and every address remembered
//!   from before is still at the same index. One that fails any of these is
//!   never used again. The message is compiled from what was just read.
//! - **The slot a table is made from.** The lookup-table program takes
//!   only a slot in the `SlotHashes` of the bank that runs the creation:
//!   one of the 512 slots before it, never its own. A slot read at the
//!   confirmed commitment may be that very bank's (the node's preflight
//!   runs at it), and one read of a node ahead of another the send reaches
//!   is in no bank there yet; either is refused as "not a recent slot". So
//!   the slot is read at the finalized commitment, of the node the creation
//!   is sent to: behind every bank that may run it, by some thirty slots,
//!   and far inside the 512 for as long as the transaction's blockhash
//!   lives. A creation still refused for its slot is made again from a
//!   slot read again.
//! - **Warm-up.** Addresses added to a table can be looked up only from the
//!   slot after the one they were added in, so after creating or extending
//!   a table the send waits for the cluster to pass that slot.
//! - **Rent.** A table costs its rent (about 0.0016 SOL, and 0.00022 SOL
//!   for each address) and is kept: it isn't deactivated and closed, so its
//!   rent is not taken back.

use anchor_lang::solana_program::instruction::Instruction;
use anyhow::{Result, anyhow, bail};
use solana_address_lookup_table_interface::state::{
    AddressLookupTable, LOOKUP_TABLE_MAX_ADDRESSES,
};
use solana_hash::Hash;
use solana_message::{AddressLookupTableAccount, Message, VersionedMessage, v0};
use solana_transaction::versioned::VersionedTransaction;

use super::Pubkey;
use super::rpc::Account;

pub use solana_address_lookup_table_interface::instruction::{
    create_lookup_table, extend_lookup_table,
};
pub use solana_address_lookup_table_interface::program::ID as PROGRAM;

/// The commitment the slot a new table is derived from is read at (see the
/// module's notes).
pub const DERIVED_FROM: &str = super::rpc::FINALIZED;
/// How many times a table's creation is made, each from a slot read again,
/// while the program refuses its slot as not recent.
pub const CREATION_TRIES: usize = 3;

/// Whether a refusal's logs are the lookup-table program refusing the slot
/// a table was to be derived from.
pub fn slot_refused(logs: &[String]) -> bool {
    logs.iter()
        .any(|line| line.contains("is not a recent slot"))
}

/// The most addresses one table holds.
pub const MOST_ADDRESSES: usize = LOOKUP_TABLE_MAX_ADDRESSES;
/// The most addresses one transaction adds to a table, so it fits the wire.
pub const ADDED_AT_ONCE: usize = 24;
/// Where a table's state says it is one, and its authority: after the
/// state's tag (four bytes), the deactivation slot, the last extended slot
/// and its start index, the authority's option tag then the key.
pub const AUTHORITY_AT: usize = 21;
const LOOKUP_TABLE_TAG: [u8; 4] = [1, 0, 0, 0];

/// A table as just read from the cluster and checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    pub address: Pubkey,
    /// Every address it holds, in order.
    pub addresses: Vec<Pubkey>,
    /// How many of them a transaction may use now: those added before the
    /// slot the table was read at.
    pub usable: usize,
    /// The slot it was last extended in.
    pub extended_in: u64,
}

impl Table {
    /// The table at `address` as `account` holds it at `slot`, if it is one
    /// `authority` may use: owned by the lookup-table program, `authority`'s,
    /// and not deactivated.
    pub fn read(address: Pubkey, account: &Account, authority: &Pubkey, slot: u64) -> Result<Self> {
        if account.owner != PROGRAM {
            bail!("{address} is not a lookup table");
        }
        let table = AddressLookupTable::deserialize(&account.data)
            .map_err(|error| anyhow!("{address} does not read as a lookup table: {error}"))?;
        if table.meta.authority != Some(*authority) {
            bail!("the lookup table {address} is not {authority}'s");
        }
        if table.meta.deactivation_slot != u64::MAX {
            bail!("the lookup table {address} is deactivated");
        }
        let usable = if slot > table.meta.last_extended_slot {
            table.addresses.len()
        } else {
            table.meta.last_extended_slot_start_index as usize
        };
        Ok(Self {
            address,
            addresses: table.addresses.to_vec(),
            usable,
            extended_in: table.meta.last_extended_slot,
        })
    }

    /// Refuse the table unless every address in `expected` is still at its
    /// index. A table only grows, so anything else is not the table it was.
    pub fn agrees_with(&self, expected: &[Pubkey]) -> Result<()> {
        if expected.len() > self.addresses.len() {
            bail!(
                "the lookup table {} holds {} addresses, fewer than the {} it held",
                self.address,
                self.addresses.len(),
                expected.len()
            );
        }
        if let Some(at) = expected
            .iter()
            .zip(&self.addresses)
            .position(|(was, is)| was != is)
        {
            bail!(
                "the lookup table {} holds {} at index {at}, where it held {}",
                self.address,
                self.addresses[at],
                expected[at]
            );
        }
        Ok(())
    }

    /// Whether it holds every one of `wanted`.
    pub fn holds(&self, wanted: &[Pubkey]) -> bool {
        wanted.iter().all(|key| self.addresses.contains(key))
    }

    /// Whether a transaction may look every one of `wanted` up in it now.
    pub fn ready_for(&self, wanted: &[Pubkey]) -> bool {
        let usable = &self.addresses[..self.usable];
        wanted.iter().all(|key| usable.contains(key))
    }

    /// Which of `wanted` it doesn't hold yet, in order.
    pub fn missing(&self, wanted: &[Pubkey]) -> Vec<Pubkey> {
        wanted
            .iter()
            .filter(|key| !self.addresses.contains(key))
            .copied()
            .collect()
    }

    /// The addresses a message may be compiled against: the usable ones.
    pub fn lookup(&self) -> AddressLookupTableAccount {
        AddressLookupTableAccount {
            key: self.address,
            addresses: self.addresses[..self.usable].to_vec(),
        }
    }
}

/// The accounts of `ixs` a table may hold for `payer`'s transaction, in the
/// order they are first named: every one not signing anywhere in it, not
/// the payer, and not a program it calls.
pub fn tableable(ixs: &[Instruction], payer: &Pubkey) -> Vec<Pubkey> {
    let signing = |key: &Pubkey| {
        key == payer
            || ixs
                .iter()
                .flat_map(|ix| &ix.accounts)
                .any(|meta| meta.is_signer && meta.pubkey == *key)
    };
    let called = |key: &Pubkey| ixs.iter().any(|ix| ix.program_id == *key);
    let mut out: Vec<Pubkey> = Vec::new();
    for meta in ixs.iter().flat_map(|ix| &ix.accounts) {
        if !signing(&meta.pubkey) && !called(&meta.pubkey) && !out.contains(&meta.pubkey) {
            out.push(meta.pubkey);
        }
    }
    out
}

/// `ixs` as a legacy message, every account named by address.
pub fn by_address(ixs: &[Instruction], payer: &Pubkey, blockhash: Hash) -> VersionedMessage {
    VersionedMessage::Legacy(Message::new_with_blockhash(ixs, Some(payer), &blockhash))
}

/// `ixs` as a version-0 message finding what it can through `table`.
pub fn through(
    ixs: &[Instruction],
    payer: &Pubkey,
    table: &AddressLookupTableAccount,
    blockhash: Hash,
) -> Result<VersionedMessage> {
    let message = v0::Message::try_compile(payer, ixs, std::slice::from_ref(table), blockhash)
        .map_err(|error| anyhow!("compiling the transaction through its lookup table: {error}"))?;
    Ok(VersionedMessage::V0(message))
}

/// `ixs` as they go: by address when that fits the wire, and otherwise
/// through `table`, which must then be given.
pub fn compile(
    ixs: &[Instruction],
    payer: &Pubkey,
    table: Option<&Table>,
    blockhash: Hash,
) -> Result<VersionedMessage> {
    let legacy = by_address(ixs, payer, blockhash);
    if fits(&legacy) {
        return Ok(legacy);
    }
    let table =
        table.ok_or_else(|| anyhow!("the transaction needs a lookup table, and has none"))?;
    let message = through(ixs, payer, &table.lookup(), blockhash)?;
    let size = wire_size(&message);
    if size > super::client::PACKET_DATA_SIZE {
        bail!(
            "a transaction of {size} bytes through a lookup table is over the wire's {}",
            super::client::PACKET_DATA_SIZE
        );
    }
    Ok(message)
}

/// How many bytes `message` takes on the wire, with its signatures.
pub fn wire_size(message: &VersionedMessage) -> usize {
    let unsigned = VersionedTransaction {
        signatures: vec![Default::default(); message.header().num_required_signatures as usize],
        message: message.clone(),
    };
    bincode::serialized_size(&unsigned).map_or(usize::MAX, |size| size as usize)
}

/// Whether `message` fits the wire.
pub fn fits(message: &VersionedMessage) -> bool {
    wire_size(message) <= super::client::PACKET_DATA_SIZE
}

/// How many bytes `ixs` take on the wire as `payer`'s transaction as it
/// would go: by address if that fits, and otherwise through one table
/// holding every account a table may hold.
pub fn sent_size(ixs: &[Instruction], payer: &Pubkey) -> Result<usize> {
    let legacy = by_address(ixs, payer, Hash::default());
    if fits(&legacy) {
        return Ok(wire_size(&legacy));
    }
    let table = AddressLookupTableAccount {
        key: Pubkey::new_from_array([0xff; 32]),
        addresses: tableable(ixs, payer),
    };
    if table.addresses.len() > MOST_ADDRESSES {
        bail!(
            "a transaction naming {} accounts is more than one lookup table holds",
            table.addresses.len()
        );
    }
    Ok(wire_size(&through(ixs, payer, &table, Hash::default())?))
}

/// Of `tables`, the one to grow to hold `wanted`: the one already holding
/// most of it that has room for the rest.
pub fn to_extend<'a>(tables: &'a [Table], wanted: &[Pubkey]) -> Option<&'a Table> {
    tables
        .iter()
        .filter(|table| table.addresses.len() + table.missing(wanted).len() <= MOST_ADDRESSES)
        .max_by_key(|table| wanted.len() - table.missing(wanted).len())
}

/// The filters that find `authority`'s tables among the lookup-table
/// program's accounts.
pub fn owned_by(authority: &Pubkey) -> Vec<(usize, Vec<u8>)> {
    let mut option = vec![1u8];
    option.extend_from_slice(authority.as_ref());
    vec![(0, LOOKUP_TABLE_TAG.to_vec()), (AUTHORITY_AT, option)]
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use anchor_lang::prelude::AccountMeta;
    use solana_address_lookup_table_interface::state::LookupTableMeta;

    use super::*;
    use crate::solana::client::PACKET_DATA_SIZE;

    /// A table's account as the lookup-table program writes it.
    fn table_account(
        authority: Option<Pubkey>,
        addresses: &[Pubkey],
        extended_in: u64,
        start: u8,
    ) -> Account {
        let state = AddressLookupTable {
            meta: LookupTableMeta {
                last_extended_slot: extended_in,
                last_extended_slot_start_index: start,
                authority,
                ..LookupTableMeta::default()
            },
            addresses: Cow::Borrowed(addresses),
        };
        Account {
            lamports: 1,
            owner: PROGRAM,
            executable: false,
            data: state.serialize_for_tests().unwrap(),
        }
    }

    fn keys(count: usize) -> Vec<Pubkey> {
        (0..count).map(|_| Pubkey::new_unique()).collect()
    }

    /// One call of `program` naming `accounts`, the first writable, with
    /// `signer` signing.
    fn call(program: Pubkey, signer: Pubkey, accounts: &[Pubkey]) -> Instruction {
        let mut metas = vec![AccountMeta::new(signer, true)];
        metas.extend(accounts.iter().enumerate().map(|(at, key)| match at {
            0 => AccountMeta::new(*key, false),
            _ => AccountMeta::new_readonly(*key, false),
        }));
        Instruction {
            program_id: program,
            accounts: metas,
            data: vec![7; 16],
        }
    }

    fn table_of(addresses: &[Pubkey]) -> Table {
        Table {
            address: Pubkey::new_unique(),
            addresses: addresses.to_vec(),
            usable: addresses.len(),
            extended_in: 0,
        }
    }

    #[test]
    fn a_transaction_that_fits_by_address_goes_as_a_legacy_one() {
        let payer = Pubkey::new_unique();
        let ixs = vec![call(Pubkey::new_unique(), payer, &keys(10))];
        let message = compile(&ixs, &payer, None, Hash::default()).unwrap();
        assert!(matches!(message, VersionedMessage::Legacy(_)));
        assert_eq!(
            sent_size(&ixs, &payer).unwrap(),
            wire_size(&message),
            "its size is its size by address"
        );
    }

    #[test]
    fn a_transaction_too_large_by_address_goes_through_its_table_with_writes_kept() {
        let payer = Pubkey::new_unique();
        let program = Pubkey::new_unique();
        let named = keys(45);
        let ixs = vec![call(program, payer, &named)];
        assert!(!fits(&by_address(&ixs, &payer, Hash::default())));
        assert!(
            compile(&ixs, &payer, None, Hash::default()).is_err(),
            "without a table it can't be sent"
        );

        let wanted = tableable(&ixs, &payer);
        assert_eq!(wanted, named, "everything but the payer and the program");
        let table = table_of(&wanted);
        let message = compile(&ixs, &payer, Some(&table), Hash::default()).unwrap();
        let VersionedMessage::V0(v0) = &message else {
            panic!("a version-0 message");
        };
        assert!(fits(&message));
        assert_eq!(sent_size(&ixs, &payer).unwrap(), wire_size(&message));
        assert_eq!(
            v0.account_keys,
            vec![payer, program],
            "only these by address"
        );
        let [lookup] = v0.address_table_lookups.as_slice() else {
            panic!("one table");
        };
        assert_eq!(lookup.account_key, table.address);
        assert_eq!(
            lookup.writable_indexes,
            vec![0],
            "the written account stays written"
        );
        assert_eq!(lookup.readonly_indexes.len(), 44);
    }

    #[test]
    fn a_signer_is_never_put_in_a_table_nor_looked_up_in_one() {
        let payer = Pubkey::new_unique();
        let other = Pubkey::new_unique();
        let named = keys(44);
        let mut ix = call(Pubkey::new_unique(), payer, &named);
        ix.accounts.push(AccountMeta::new_readonly(other, true));
        // The same key named again without signing still signs.
        ix.accounts.push(AccountMeta::new_readonly(other, false));
        let ixs = vec![ix];
        let wanted = tableable(&ixs, &payer);
        assert!(!wanted.contains(&payer) && !wanted.contains(&other));

        // Even a table holding the signers doesn't have them looked up.
        let mut held = wanted.clone();
        held.extend([payer, other]);
        let message = compile(&ixs, &payer, Some(&table_of(&held)), Hash::default()).unwrap();
        let VersionedMessage::V0(v0) = &message else {
            panic!("a version-0 message");
        };
        assert_eq!(v0.header.num_required_signatures, 2);
        assert!(v0.account_keys[..2].contains(&payer) && v0.account_keys[..2].contains(&other));
        let looked_up: Vec<u8> = v0.address_table_lookups[0]
            .writable_indexes
            .iter()
            .chain(&v0.address_table_lookups[0].readonly_indexes)
            .copied()
            .collect();
        assert!(!looked_up.contains(&(held.len() as u8 - 2)));
        assert!(!looked_up.contains(&(held.len() as u8 - 1)));
    }

    #[test]
    fn a_table_is_used_only_if_it_is_the_payer_s_and_live() {
        let me = Pubkey::new_unique();
        let address = Pubkey::new_unique();
        let held = keys(3);
        let mine = table_account(Some(me), &held, 5, 0);
        let table = Table::read(address, &mine, &me, 9).unwrap();
        assert_eq!(table.addresses, held);
        assert_eq!(table.usable, 3);

        let theirs = table_account(Some(Pubkey::new_unique()), &held, 5, 0);
        assert!(Table::read(address, &theirs, &me, 9).is_err());
        let frozen = table_account(None, &held, 5, 0);
        assert!(Table::read(address, &frozen, &me, 9).is_err());
        let mut elsewhere = mine.clone();
        elsewhere.owner = Pubkey::new_unique();
        assert!(Table::read(address, &elsewhere, &me, 9).is_err());
        let mut deactivated = AddressLookupTable::deserialize(&mine.data).unwrap().meta;
        deactivated.deactivation_slot = 8;
        let mut data = mine.data.clone();
        AddressLookupTable::overwrite_meta_data(&mut data, deactivated).unwrap();
        let deactivated = Account { data, ..mine };
        assert!(Table::read(address, &deactivated, &me, 9).is_err());
    }

    #[test]
    fn addresses_added_in_this_slot_are_not_used_until_the_next() {
        let me = Pubkey::new_unique();
        let held = keys(5);
        let account = table_account(Some(me), &held, 9, 2);
        let now = Table::read(Pubkey::new_unique(), &account, &me, 9).unwrap();
        assert_eq!(now.usable, 2);
        assert!(now.holds(&held) && !now.ready_for(&held));
        assert!(now.ready_for(&held[..2]));
        assert_eq!(now.lookup().addresses, held[..2].to_vec());
        let next = Table::read(Pubkey::new_unique(), &account, &me, 10).unwrap();
        assert!(next.ready_for(&held));
    }

    #[test]
    fn a_table_with_another_address_at_an_index_it_held_is_refused() {
        let me = Pubkey::new_unique();
        let held = keys(4);
        let grown = [held.clone(), keys(2)].concat();
        let table = Table::read(
            Pubkey::new_unique(),
            &table_account(Some(me), &grown, 1, 0),
            &me,
            2,
        )
        .unwrap();
        table.agrees_with(&held).unwrap();
        table.agrees_with(&grown).unwrap();

        let mut swapped = held.clone();
        swapped[2] = Pubkey::new_unique();
        let error = table.agrees_with(&swapped).unwrap_err().to_string();
        assert!(error.contains("index 2"), "{error}");
        let mut longer = grown.clone();
        longer.push(Pubkey::new_unique());
        assert!(table.agrees_with(&longer).is_err());
    }

    #[test]
    fn a_table_is_grown_where_most_of_a_transaction_already_is_and_has_room() {
        let wanted = keys(10);
        let some = table_of(&wanted[..3]);
        let most = table_of(&[&wanted[..8], &keys(2)[..]].concat());
        let full = table_of(&[&wanted[..9], &keys(MOST_ADDRESSES - 9)[..]].concat());
        let tables = [some.clone(), most.clone(), full];
        assert_eq!(to_extend(&tables, &wanted), Some(&most));
        assert_eq!(most.missing(&wanted), wanted[8..].to_vec());
        assert_eq!(to_extend(&[], &wanted), None);
    }

    #[test]
    fn an_executor_call_naming_eight_markets_and_eight_orders_fits_through_a_table() {
        use anchor_lang::{InstructionData, ToAccountMetas};
        // A trade through the station's guard: its records, then the
        // venue's place with eight orders across eight markets.
        let key = Pubkey::new_unique();
        let authority = Pubkey::new_unique();
        let [station, station_dollars, authority_dollars, mint] =
            [(); 4].map(|_| Pubkey::new_unique());
        let [config, blessing, key_info, image, project, slot, revoked] =
            [(); 7].map(|_| Pubkey::new_unique());
        let venue = shonin_local_venue::ID;
        let markets = keys(8);
        let orders: Vec<shonin_local_venue::OrderRequest> = (0..8u8)
            .map(|at| shonin_local_venue::OrderRequest {
                market: [at; 32],
                is_buy: at % 2 == 0,
                size: 1_000_000_000_000_000_000,
                limit_price: 1_000_000,
                reduce_only: false,
            })
            .collect();
        let mut accounts = shonin_station::accounts::Execute {
            key,
            station,
            station_dollars,
            authority,
            authority_dollars,
            mint,
            token_program: anchor_spl::token::ID,
            registry_config: config,
            blessing,
            key_info,
            image,
            project,
            slot,
            revoked,
        }
        .to_account_metas(None);
        accounts.push(AccountMeta::new_readonly(venue, false));
        accounts.extend(
            shonin_local_venue::accounts::Place {
                master: authority,
                account: shonin_local_venue::account_address(&authority),
                venue: shonin_local_venue::venue_address(),
            }
            .to_account_metas(None)
            .into_iter()
            .map(|mut meta| {
                meta.is_signer = false;
                meta
            }),
        );
        accounts.extend(
            markets
                .iter()
                .map(|market| AccountMeta::new_readonly(*market, false)),
        );
        let ix = Instruction {
            program_id: shonin_station::ID,
            accounts,
            data: shonin_station::instruction::Execute {
                driver: 0,
                data: shonin_local_venue::instruction::Place { orders }.data(),
                fund: 0,
            }
            .data(),
        };
        let ixs = vec![
            solana_compute_budget_interface::ComputeBudgetInstruction::set_compute_unit_limit(1),
            solana_compute_budget_interface::ComputeBudgetInstruction::request_heap_frame(
                256 * 1024,
            ),
            ix,
        ];
        let legacy = wire_size(&by_address(&ixs, &key, Hash::default()));
        assert!(legacy > PACKET_DATA_SIZE, "{legacy} bytes by address");
        let wanted = tableable(&ixs, &key);
        let message = compile(&ixs, &key, Some(&table_of(&wanted)), Hash::default()).unwrap();
        assert!(fits(&message));
        assert_eq!(sent_size(&ixs, &key).unwrap(), wire_size(&message));
    }

    #[test]
    fn a_table_is_found_by_its_authority_where_the_program_writes_it() {
        let me = Pubkey::new_unique();
        let account = table_account(Some(me), &keys(2), 0, 0);
        for (at, bytes) in owned_by(&me) {
            assert_eq!(&account.data[at..at + bytes.len()], bytes.as_slice());
        }
        let theirs = table_account(Some(Pubkey::new_unique()), &keys(2), 0, 0);
        let (at, bytes) = &owned_by(&me)[1];
        assert_ne!(&theirs.data[*at..at + bytes.len()], bytes.as_slice());
    }
}
