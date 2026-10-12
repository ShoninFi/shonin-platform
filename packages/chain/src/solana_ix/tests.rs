//! Every builder against the programs' own crates: for the same inputs, the
//! same program, the same accounts in the same order with the same signer
//! and writable flags, and the same data, byte for byte. Where the services
//! already build an instruction, the program's types are filled in here as
//! the services fill them in.

use anchor_lang::{AccountSerialize, Discriminator, InstructionData, ToAccountMetas};
use anchor_spl::associated_token::get_associated_token_address;
use shonin_vault::state as vs;

use super::transaction::{AddressLookupTableAccount, TransactionError, Unsigned};
use super::vault::{
    Activation, Allowances, ClaimKeys, Driver, Groups, Held, KeyRecords, Pinned, RequestKind,
    SignedUsage, VaultKeys,
};
use super::{AccountMeta, Hash, Instruction, Pubkey, address, budget, ids, publishing, read};

const SYSTEM: Pubkey = anchor_lang::solana_program::system_program::ID;
const TOKEN: Pubkey = anchor_spl::token::ID;

/// An instruction as the program's own types make it.
fn anchor(
    program_id: Pubkey,
    accounts: impl ToAccountMetas,
    data: impl InstructionData,
) -> Instruction {
    Instruction {
        program_id,
        accounts: accounts.to_account_metas(None),
        data: data.data(),
    }
}

/// The same with accounts after the program's own (driver groups, earlier
/// factories).
fn anchor_with(
    program_id: Pubkey,
    accounts: impl ToAccountMetas,
    rest: &[AccountMeta],
    data: impl InstructionData,
) -> Instruction {
    let mut ix = anchor(program_id, accounts, data);
    ix.accounts.extend_from_slice(rest);
    ix
}

fn key() -> Pubkey {
    Pubkey::new_unique()
}

/// A vault with every key distinct.
fn a_vault() -> VaultKeys {
    VaultKeys::new(7, key(), key(), key())
}

/// `owner`'s dollar account as the services find it.
fn dollars(owner: &Pubkey, v: &VaultKeys) -> Pubkey {
    get_associated_token_address(owner, &v.mint)
}

/// The program's `Pool` for `v`, as the services fill it in.
fn pool(v: &VaultKeys) -> shonin_vault::accounts::Pool {
    shonin_vault::accounts::Pool {
        vault: v.vault,
        station: v.station,
        author_holder: vs::holder_address(&v.vault, &v.author).0,
        platform_holder: vs::holder_address(&v.vault, &v.platform).0,
        vault_dollars: dollars(&v.vault, v),
        mint: v.mint,
        token_program: TOKEN,
        system_program: SYSTEM,
    }
}

fn event_authority() -> Pubkey {
    Pubkey::find_program_address(&[b"__event_authority"], &shonin_vault::ID).0
}

/// Two driver groups of different sizes, some accounts written.
fn two_groups() -> Groups {
    let mut groups = Groups::none();
    groups.push(vec![
        AccountMeta::new_readonly(key(), false),
        AccountMeta::new(key(), false),
    ]);
    groups.push(vec![
        AccountMeta::new_readonly(key(), false),
        AccountMeta::new(key(), false),
        AccountMeta::new_readonly(key(), false),
    ]);
    groups
}

// Ids, discriminators and addresses ------------------------------------------------------

#[test]
fn the_program_ids_compiled_in_are_the_programs_own_for_the_same_network() {
    assert_eq!(ids::REGISTRY, shonin_registry::ID);
    assert_eq!(ids::PUBLISHING, shonin_publishing::ID);
    assert_eq!(ids::PRICES, shonin_prices::ID);
    assert_eq!(ids::STATION, shonin_station::ID);
    assert_eq!(ids::VAULT, shonin_vault::ID);
    assert_eq!(ids::LOCAL_VENUE, shonin_local_venue::ID);
    assert_eq!(ids::LOCAL_DRIVER, shonin_local_driver::ID);
}

#[cfg(feature = "devnet")]
#[test]
fn a_devnet_build_names_the_program_ids_committed_for_devnet() {
    macro_rules! committed {
        ($program:literal) => {
            include_str!(concat!(
                "../../../../contracts/solana/programs/",
                $program,
                "/devnet-id"
            ))
            .trim()
            .parse::<Pubkey>()
            .unwrap()
        };
    }
    assert_eq!(ids::REGISTRY, committed!("shonin-registry"));
    assert_eq!(ids::PUBLISHING, committed!("shonin-publishing"));
    assert_eq!(ids::PRICES, committed!("shonin-prices"));
    assert_eq!(ids::STATION, committed!("shonin-station"));
    assert_eq!(ids::VAULT, committed!("shonin-vault"));
    assert_eq!(ids::LOCAL_VENUE, committed!("shonin-local-venue"));
    assert_eq!(ids::LOCAL_DRIVER, committed!("shonin-local-driver"));
}

#[test]
fn the_solana_programs_named_are_the_sdk_s_own() {
    use anchor_lang::solana_program as sp;
    assert_eq!(ids::SYSTEM, sp::system_program::ID);
    assert_eq!(ids::TOKEN, anchor_spl::token::ID);
    assert_eq!(ids::ASSOCIATED_TOKEN, anchor_spl::associated_token::ID);
    assert_eq!(ids::COMPUTE_BUDGET, solana_compute_budget_interface::id());
    assert_eq!(
        ids::ED25519,
        solana_ed25519_program::new_ed25519_instruction_with_signature(&[], &[0; 64], &[0; 32])
            .program_id
    );
    assert_eq!(ids::INSTRUCTIONS, sp::sysvar::instructions::ID);
    assert_eq!(ids::UPGRADEABLE_LOADER, sp::bpf_loader_upgradeable::ID);
}

#[test]
fn every_derived_address_is_the_one_the_programs_derive() {
    let (vault, owner, signer, program) = (key(), key(), key(), key());
    let (mint, digest, name) = (key(), [3u8; 32], [4u8; 32]);
    assert_eq!(address::factory(), vs::factory_address());
    assert_eq!(address::vault(9), vs::vault_address(9).0);
    assert_eq!(
        address::vault_of(&program, 9),
        vs::vault_address_of(&program, 9).0
    );
    assert_eq!(
        address::factory_of(&program),
        Pubkey::find_program_address(&[vs::FACTORY_SEED], &program).0
    );
    assert_eq!(
        address::holder(&vault, &owner),
        vs::holder_address(&vault, &owner).0
    );
    assert_eq!(
        address::request(&vault, &owner, 5),
        vs::request_address(&vault, &owner, 5).0
    );
    assert_eq!(address::claim(&vault, 5), vs::claim_address(&vault, 5).0);
    assert_eq!(
        address::restricted(&vault),
        vs::restricted_address(&vault).0
    );
    assert_eq!(address::event_authority(), event_authority());
    assert_eq!(
        address::station(&vault),
        shonin_station::station_address(&vault)
    );
    assert_eq!(address::station_config(), shonin_station::config_address());
    assert_eq!(
        address::venue_authority(&vault, &program),
        shonin_station::venue_authority(&vault, &program).0
    );
    let publishing = |seed: &[u8], id: u64| {
        Pubkey::find_program_address(&[seed, &id.to_le_bytes()], &shonin_publishing::ID).0
    };
    use shonin_publishing::state as ps;
    assert_eq!(
        address::publishing_config(),
        Pubkey::find_program_address(&[ps::CONFIG_SEED], &shonin_publishing::ID).0
    );
    assert_eq!(address::submission(4), publishing(ps::SUBMISSION_SEED, 4));
    assert_eq!(address::strategy(4), ps::strategy_address(4));
    assert_eq!(address::review(4), publishing(ps::REVIEW_SEED, 4));
    use shonin_registry::read as registry;
    use shonin_registry::state::ApprovalKind;
    assert_eq!(address::registry_config(), registry::config_address());
    assert_eq!(address::key(&signer), registry::key_address(&signer));
    assert_eq!(address::image(&digest), registry::image_address(&digest));
    assert_eq!(
        address::project(&digest),
        registry::project_address(&digest)
    );
    assert_eq!(
        address::blessing(&signer, &digest, &vault, &name),
        registry::blessing_address(&signer, &digest, &vault.to_bytes(), &name)
    );
    assert_eq!(
        address::driver_approval(&name),
        registry::approval_address(ApprovalKind::Driver, &name)
    );
    assert_eq!(
        address::driver_revoked(&name, &program),
        registry::revoked_address(ApprovalKind::Driver, &name, &program)
    );
    assert_eq!(address::prices(), shonin_prices::read::prices_address());
    assert_eq!(
        address::program_data(&program),
        Pubkey::find_program_address(
            &[program.as_ref()],
            &anchor_lang::solana_program::bpf_loader_upgradeable::ID
        )
        .0
    );
    use shonin_local_venue as venue;
    assert_eq!(
        address::venue_state(&program),
        venue::venue_address_of(&program)
    );
    assert_eq!(
        address::venue_vault(&program),
        venue::vault_address_of(&program)
    );
    assert_eq!(
        address::venue_account(&program, &owner),
        venue::account_address_of(&program, &owner)
    );
    assert_eq!(
        address::venue_market(&program, &digest),
        venue::market_address_of(&program, &digest)
    );
    assert_eq!(
        address::dollars(&owner, &mint),
        get_associated_token_address(&owner, &mint)
    );
    let token_2022 = anchor_spl::token_2022::ID;
    assert_eq!(
        address::associated_token(&owner, &mint, &token_2022),
        anchor_spl::associated_token::get_associated_token_address_with_program_id(
            &owner,
            &mint,
            &token_2022
        )
    );
}

#[test]
fn a_vault_s_keys_are_its_station_and_dollar_accounts_as_the_services_find_them() {
    let v = a_vault();
    let vault = vs::vault_address(7).0;
    let station = shonin_station::station_address(&vault);
    assert_eq!(v.vault, vault);
    assert_eq!(v.station, station);
    assert_eq!(v.vault_dollars, dollars(&vault, &v));
    assert_eq!(v.station_dollars, dollars(&station, &v));
}

// Depositors ------------------------------------------------------------------------------

#[test]
fn a_deposit_with_its_statement_is_the_program_s_request_deposit() {
    let v = a_vault();
    let (payer, receiver) = (key(), key());
    for statement in [Some([9u8; 32]), None] {
        let ix = super::vault::request_deposit(&v, &payer, &receiver, 1_234_567, statement, 3);
        let expected = anchor(
            shonin_vault::ID,
            shonin_vault::accounts::RequestDeposit {
                payer,
                vault: v.vault,
                holder: vs::holder_address(&v.vault, &receiver).0,
                request: vs::request_address(&v.vault, &payer, 3).0,
                payer_dollars: dollars(&payer, &v),
                vault_dollars: dollars(&v.vault, &v),
                mint: v.mint,
                token_program: TOKEN,
                system_program: SYSTEM,
            },
            shonin_vault::instruction::RequestDeposit {
                assets: 1_234_567,
                receiver,
                statement,
                nonce: 3,
            },
        );
        assert_eq!(ix, expected);
    }
}

#[test]
fn a_statement_alone_is_the_program_s_record_statement() {
    let v = a_vault();
    let owner = key();
    assert_eq!(
        super::vault::record_statement(&v, &owner, [5; 32]),
        anchor(
            shonin_vault::ID,
            shonin_vault::accounts::RecordStatement {
                owner,
                vault: v.vault,
                holder: vs::holder_address(&v.vault, &owner).0,
                system_program: SYSTEM,
            },
            shonin_vault::instruction::RecordStatement { statement: [5; 32] },
        )
    );
}

#[test]
fn a_withdrawal_is_the_program_s_request_redeem() {
    let v = a_vault();
    let (owner, receiver) = (key(), key());
    let shares = u128::MAX >> 20;
    assert_eq!(
        super::vault::request_redeem(&v, &owner, shares, &receiver, 11),
        anchor(
            shonin_vault::ID,
            shonin_vault::accounts::RequestRedeem {
                owner,
                vault: v.vault,
                holder: vs::holder_address(&v.vault, &owner).0,
                request: vs::request_address(&v.vault, &owner, 11).0,
                system_program: SYSTEM,
            },
            shonin_vault::instruction::RequestRedeem {
                shares,
                receiver,
                nonce: 11,
            },
        )
    );
}

#[test]
fn booking_a_claim_instead_is_the_program_s_claim_instead() {
    let v = a_vault();
    let (owner, request) = (key(), key());
    assert_eq!(
        super::vault::claim_instead(&v, &owner, &request, 12),
        anchor(
            shonin_vault::ID,
            shonin_vault::accounts::ClaimInstead {
                owner,
                vault: v.vault,
                request,
                claim: vs::claim_address(&v.vault, 12).0,
                system_program: SYSTEM,
            },
            shonin_vault::instruction::ClaimInstead { request_id: 12 },
        )
    );
}

#[test]
fn settling_a_claim_is_the_program_s_settle_claim_with_the_station_s_groups_after() {
    let v = a_vault();
    let caller = key();
    let claim = ClaimKeys {
        id: 21,
        owner: key(),
        request: 2,
    };
    let groups = two_groups();
    let claim_address = vs::claim_address(&v.vault, 21).0;
    let expected = anchor_with(
        shonin_vault::ID,
        shonin_vault::accounts::SettleClaim {
            caller,
            pool: pool(&v),
            close: shonin_vault::accounts::Closing {
                station_program: shonin_station::ID,
                station_dollars: dollars(&v.station, &v),
            },
            claim: claim_address,
            request: Some(vs::request_address(&v.vault, &claim_address, 2).0),
            owner_dollars: dollars(&claim.owner, &v),
            author_dollars: dollars(&v.author, &v),
            platform_dollars: dollars(&v.platform, &v),
            event_authority: event_authority(),
            program: shonin_vault::ID,
        },
        &groups.accounts,
        shonin_vault::instruction::SettleClaim {
            claim_id: 21,
            groups: vec![2, 3],
        },
    );
    assert_eq!(
        super::vault::settle_claim(&v, &caller, &claim, &groups),
        expected
    );
}

#[test]
fn the_pro_rata_fallback_is_the_program_s_take_pro_rata_with_or_without_groups() {
    let v = a_vault();
    let (owner, request) = (key(), key());
    for groups in [Groups::none(), two_groups()] {
        let expected = anchor_with(
            shonin_vault::ID,
            shonin_vault::accounts::TakeProRata {
                owner,
                pool: pool(&v),
                station_program: shonin_station::ID,
                request,
                holder: vs::holder_address(&v.vault, &owner).0,
                to_dollars: dollars(&owner, &v),
                system_program: SYSTEM,
                event_authority: event_authority(),
                program: shonin_vault::ID,
            },
            &groups.accounts,
            shonin_vault::instruction::TakeProRata {
                request_id: 30,
                groups: groups.sizes.clone(),
            },
        );
        assert_eq!(
            super::vault::take_pro_rata(&v, &owner, &request, 30, &groups),
            expected
        );
    }
}

#[test]
fn a_claim_s_fallback_is_the_program_s_take_pro_rata_claim_queued_or_not() {
    let v = a_vault();
    let claim = ClaimKeys {
        id: 40,
        owner: key(),
        request: 6,
    };
    let claim_address = vs::claim_address(&v.vault, 40).0;
    for queued in [true, false] {
        let expected = anchor(
            shonin_vault::ID,
            shonin_vault::accounts::TakeProRataClaim {
                owner: claim.owner,
                pool: pool(&v),
                claim: claim_address,
                request: queued.then(|| vs::request_address(&v.vault, &claim_address, 6).0),
                holder: vs::holder_address(&v.vault, &claim.owner).0,
                to_dollars: dollars(&claim.owner, &v),
                system_program: SYSTEM,
            },
            shonin_vault::instruction::TakeProRataClaim { claim_id: 40 },
        );
        assert_eq!(
            super::vault::take_pro_rata_claim(&v, &claim, queued),
            expected
        );
    }
}

#[test]
fn collecting_units_is_the_program_s_collect_units() {
    let v = a_vault();
    let owner = key();
    assert_eq!(
        super::vault::collect_units(&v, &owner),
        anchor(
            shonin_vault::ID,
            shonin_vault::accounts::CollectUnits {
                owner,
                vault: v.vault,
                holder: vs::holder_address(&v.vault, &owner).0,
                to: dollars(&owner, &v),
                vault_dollars: dollars(&v.vault, &v),
                mint: v.mint,
                token_program: TOKEN,
            },
            shonin_vault::instruction::CollectUnits {},
        )
    );
}

#[test]
fn collecting_a_held_payout_is_the_program_s_collect_held_for_each_place_it_is_held() {
    use shonin_vault::upkeep::HeldIn;
    let v = a_vault();
    let (owner, request) = (key(), key());
    for (held, theirs, record) in [
        (Held::Request(3), HeldIn::Request(3), Some(request)),
        (
            Held::Claim(4),
            HeldIn::Claim(4),
            Some(vs::claim_address(&v.vault, 4).0),
        ),
        (Held::Fees, HeldIn::Fees, None),
    ] {
        let expected = anchor(
            shonin_vault::ID,
            shonin_vault::accounts::CollectHeld {
                owner,
                vault: v.vault,
                record,
                to: dollars(&owner, &v),
                vault_dollars: dollars(&v.vault, &v),
                mint: v.mint,
                token_program: TOKEN,
            },
            shonin_vault::instruction::CollectHeld { held: theirs },
        );
        assert_eq!(
            super::vault::collect_held(&v, &owner, held, Some(request)),
            expected
        );
    }
}

// A closed vault ---------------------------------------------------------------------------

#[test]
fn a_closed_vault_s_redemption_is_the_program_s_redeem_closed() {
    let v = a_vault();
    let (owner, receiver) = (key(), key());
    assert_eq!(
        super::vault::redeem_closed(&v, &owner, 77, &receiver),
        anchor(
            shonin_vault::ID,
            shonin_vault::accounts::RedeemClosed {
                owner,
                vault: v.vault,
                holder: vs::holder_address(&v.vault, &owner).0,
                station: v.station,
                to: dollars(&receiver, &v),
                vault_dollars: dollars(&v.vault, &v),
                mint: v.mint,
                token_program: TOKEN,
            },
            shonin_vault::instruction::RedeemClosed {
                shares: 77,
                receiver,
            },
        )
    );
}

#[test]
fn a_closed_vault_s_claim_payout_is_the_program_s_claim_closed_queued_or_not() {
    let v = a_vault();
    let claim = ClaimKeys {
        id: 8,
        owner: key(),
        request: 1,
    };
    let address = vs::claim_address(&v.vault, 8).0;
    for queued in [true, false] {
        let expected = anchor(
            shonin_vault::ID,
            shonin_vault::accounts::ClosedDoor {
                vault: v.vault,
                station: v.station,
                record: address,
                other: queued.then(|| vs::request_address(&v.vault, &address, 1).0),
                to: dollars(&claim.owner, &v),
                vault_dollars: dollars(&v.vault, &v),
                mint: v.mint,
                token_program: TOKEN,
            },
            shonin_vault::instruction::ClaimClosed { claim_id: 8 },
        );
        assert_eq!(super::vault::claim_closed(&v, &claim, queued), expected);
    }
}

#[test]
fn a_closed_vault_settles_each_kind_of_request_as_the_program_s_settle_closed() {
    let v = a_vault();
    let (request, owner, payer) = (key(), key(), key());
    for (kind, other, to) in [
        (
            RequestKind::Deposit,
            Some(vs::holder_address(&v.vault, &owner).0),
            dollars(&payer, &v),
        ),
        (RequestKind::Redeem, None, dollars(&owner, &v)),
        (RequestKind::Claim, None, dollars(&v.author, &v)),
        (RequestKind::TankFill, None, dollars(&v.author, &v)),
    ] {
        let expected = anchor(
            shonin_vault::ID,
            shonin_vault::accounts::ClosedDoor {
                vault: v.vault,
                station: v.station,
                record: request,
                other,
                to,
                vault_dollars: dollars(&v.vault, &v),
                mint: v.mint,
                token_program: TOKEN,
            },
            shonin_vault::instruction::SettleClosed { request_id: 14 },
        );
        assert_eq!(
            super::vault::settle_closed(&v, &request, 14, kind, &owner, &payer),
            expected
        );
    }
}

#[test]
fn rent_back_is_the_program_s_close_request_close_claim_and_close_holder() {
    let v = a_vault();
    let (request, payer, owner) = (key(), key(), key());
    assert_eq!(
        super::vault::close_request(&v, &request, 3, &payer),
        anchor(
            shonin_vault::ID,
            shonin_vault::accounts::CloseRecord {
                vault: v.vault,
                record: request,
                payer,
            },
            shonin_vault::instruction::CloseRequest { request_id: 3 },
        )
    );
    assert_eq!(
        super::vault::close_claim(&v, 4, &payer),
        anchor(
            shonin_vault::ID,
            shonin_vault::accounts::CloseRecord {
                vault: v.vault,
                record: vs::claim_address(&v.vault, 4).0,
                payer,
            },
            shonin_vault::instruction::CloseClaim { claim_id: 4 },
        )
    );
    assert_eq!(
        super::vault::close_holder(&v, &owner, &payer),
        anchor(
            shonin_vault::ID,
            shonin_vault::accounts::CloseHolder {
                owner,
                vault: v.vault,
                holder: vs::holder_address(&v.vault, &owner).0,
                payer,
            },
            shonin_vault::instruction::CloseHolder {},
        )
    );
}

// The tank ----------------------------------------------------------------------------------

#[test]
fn funding_the_tank_is_the_program_s_fund_tank() {
    let v = a_vault();
    let funder = key();
    assert_eq!(
        super::vault::fund_tank(&v, &funder, 5_000_000),
        anchor(
            shonin_vault::ID,
            shonin_vault::accounts::FundTank {
                funder,
                vault: v.vault,
                from: dollars(&funder, &v),
                vault_dollars: dollars(&v.vault, &v),
                mint: v.mint,
                token_program: TOKEN,
            },
            shonin_vault::instruction::FundTank { assets: 5_000_000 },
        )
    );
}

fn signed_usage() -> (SignedUsage, KeyRecords) {
    (
        SignedUsage {
            total: 4_321,
            as_of: 1_700_000_000,
            signer: key(),
            signature: [6; 64],
        },
        KeyRecords {
            blessing: key(),
            key_info: key(),
            image: key(),
            project: key(),
        },
    )
}

fn billing(v: &VaultKeys, records: Option<&KeyRecords>) -> shonin_vault::accounts::Billing {
    let config = shonin_registry::read::config_address();
    shonin_vault::accounts::Billing {
        registry_config: config,
        blessing: records.map_or(config, |r| r.blessing),
        key_info: records.map_or(config, |r| r.key_info),
        image: records.map_or(config, |r| r.image),
        project: records.map_or(config, |r| r.project),
        prices: shonin_prices::read::prices_address(),
        instructions: anchor_lang::solana_program::sysvar::instructions::ID,
        platform_dollars: dollars(&v.platform, v),
    }
}

#[test]
fn a_signed_total_travels_in_the_ed25519_instruction_the_sdk_makes_over_the_vault_s_digest() {
    let v = a_vault();
    let (signed, _) = signed_usage();
    let cluster = shonin_common::digest::cluster_value("devnet");
    let digest = shonin_vault::upkeep::usage_digest(&cluster, &v.vault, signed.total, signed.as_of);
    assert_eq!(
        read::usage_digest(&cluster, &v.vault, signed.total, signed.as_of),
        digest
    );
    assert_eq!(
        super::vault::usage_signature(&cluster, &v.vault, &signed),
        solana_ed25519_program::new_ed25519_instruction_with_signature(
            &digest,
            &signed.signature,
            &signed.signer.to_bytes()
        )
    );
}

#[test]
fn keep_is_the_program_s_keep_after_the_signature_it_collects_with() {
    let v = a_vault();
    let caller = key();
    let cluster = shonin_common::digest::cluster_value("devnet");
    let (signed, records) = signed_usage();
    let request = Some(vs::request_address(&v.vault, &v.vault, 9).0);
    let keep = |records: Option<&KeyRecords>, usage| {
        anchor(
            shonin_vault::ID,
            shonin_vault::accounts::Keep {
                caller,
                vault: v.vault,
                request,
                billing: billing(&v, records),
                vault_dollars: dollars(&v.vault, &v),
                mint: v.mint,
                token_program: TOKEN,
                system_program: SYSTEM,
            },
            shonin_vault::instruction::Keep { usage },
        )
    };
    assert_eq!(
        super::vault::keep(&v, &caller, 9, None, &cluster),
        vec![keep(None, None)]
    );
    let usage = shonin_vault::Usage {
        total: signed.total,
        as_of: signed.as_of,
        signer: signed.signer,
    };
    assert_eq!(
        super::vault::keep(&v, &caller, 9, Some((&signed, &records)), &cluster),
        vec![
            super::vault::usage_signature(&cluster, &v.vault, &signed),
            keep(Some(&records), Some(usage)),
        ]
    );
}

#[test]
fn a_signed_tank_withdrawal_is_the_program_s_withdraw_tank_after_its_signature() {
    let v = a_vault();
    let cluster = shonin_common::digest::cluster_value("devnet");
    let (signed, records) = signed_usage();
    let expected = anchor(
        shonin_vault::ID,
        shonin_vault::accounts::WithdrawTank {
            author: v.author,
            vault: v.vault,
            billing: billing(&v, Some(&records)),
            to: dollars(&v.author, &v),
            vault_dollars: dollars(&v.vault, &v),
            mint: v.mint,
            token_program: TOKEN,
        },
        shonin_vault::instruction::WithdrawTank {
            assets: 1_000,
            usage: shonin_vault::Usage {
                total: signed.total,
                as_of: signed.as_of,
                signer: signed.signer,
            },
        },
    );
    assert_eq!(
        super::vault::withdraw_tank(&v, 1_000, &signed, &records, &cluster),
        vec![
            super::vault::usage_signature(&cluster, &v.vault, &signed),
            expected
        ]
    );
}

#[test]
fn the_unsigned_tank_escape_is_the_program_s_request_then_withdraw_tank_unsigned() {
    let v = a_vault();
    let author = shonin_vault::accounts::Author {
        author: v.author,
        vault: v.vault,
    };
    assert_eq!(
        super::vault::request_tank_withdrawal(&v),
        anchor(
            shonin_vault::ID,
            author,
            shonin_vault::instruction::RequestTankWithdrawal {},
        )
    );
    assert_eq!(
        super::vault::withdraw_tank_unsigned(&v, 2_500),
        anchor(
            shonin_vault::ID,
            shonin_vault::accounts::WithdrawTankUnsigned {
                author: v.author,
                vault: v.vault,
                to: dollars(&v.author, &v),
                vault_dollars: dollars(&v.vault, &v),
                mint: v.mint,
                token_program: TOKEN,
            },
            shonin_vault::instruction::WithdrawTankUnsigned { assets: 2_500 },
        )
    );
}

#[test]
fn topping_up_from_shares_is_the_program_s_top_up_from_shares() {
    let v = a_vault();
    assert_eq!(
        super::vault::top_up_from_shares(&v, 123_456_789, 4),
        anchor(
            shonin_vault::ID,
            shonin_vault::accounts::TopUpFromShares {
                author: v.author,
                vault: v.vault,
                request: vs::request_address(&v.vault, &v.author, 4).0,
                system_program: SYSTEM,
            },
            shonin_vault::instruction::TopUpFromShares {
                shares: 123_456_789,
                nonce: 4,
            },
        )
    );
}

// The author ---------------------------------------------------------------------------------

#[test]
fn the_author_s_settings_are_the_program_s_own_instructions() {
    let v = a_vault();
    let author = || shonin_vault::accounts::Author {
        author: v.author,
        vault: v.vault,
    };
    assert_eq!(
        super::vault::set_tank_keep(&v, 50_000_000),
        anchor(
            shonin_vault::ID,
            author(),
            shonin_vault::instruction::SetTankKeep { assets: 50_000_000 },
        )
    );
    assert_eq!(
        super::vault::set_idle_floor(&v, 2_500),
        anchor(
            shonin_vault::ID,
            author(),
            shonin_vault::instruction::SetIdleFloor { bps: 2_500 },
        )
    );
    assert_eq!(
        super::vault::retire(&v),
        anchor(
            shonin_vault::ID,
            author(),
            shonin_vault::instruction::Retire {}
        )
    );
}

fn allowances() -> (Allowances, shonin_prices::Allowances) {
    (
        Allowances {
            fuel_per_sec: 1_000_000,
            memory_mib: 256,
            requests_per_min: 60,
            bytes_per_day: 1 << 30,
            streams: 4,
            inference_ms_per_min: 500,
            gas_per_day: 2_000_000,
        },
        shonin_prices::Allowances {
            fuel_per_sec: 1_000_000,
            memory_mib: 256,
            requests_per_min: 60,
            bytes_per_day: 1 << 30,
            streams: 4,
            inference_ms_per_min: 500,
            gas_per_day: 2_000_000,
        },
    )
}

#[test]
fn setting_allowances_is_the_program_s_set_allowances() {
    let v = a_vault();
    let (ours, theirs) = allowances();
    assert_eq!(
        super::vault::set_allowances(&v, &ours),
        anchor(
            shonin_vault::ID,
            shonin_vault::accounts::SetAllowances {
                author: v.author,
                vault: v.vault,
                prices: shonin_prices::read::prices_address(),
            },
            shonin_vault::instruction::SetAllowances { allowances: theirs },
        )
    );
}

#[test]
fn activation_is_the_program_s_activate_with_earlier_factories_then_driver_groups() {
    let v = a_vault();
    let (ours, theirs) = allowances();
    let drivers = vec![
        Driver {
            name: [1; 32],
            program: key(),
            venue: key(),
        },
        Driver {
            name: [2; 32],
            program: key(),
            venue: key(),
        },
    ];
    let previous = vec![key(), key()];
    let activation = Activation {
        strategy_id: 7,
        allowances: ours,
        idle_floor_bps: 1_000,
        tank_funding: 20_000_000,
        tank_keep: 5_000_000,
        statement: [8; 32],
        terms: [9; 32],
        drivers: drivers.clone(),
        previous: previous.clone(),
    };
    use shonin_registry::read as registry;
    use shonin_registry::state::ApprovalKind;
    let mut rest = Vec::new();
    for program in &previous {
        rest.push(AccountMeta::new_readonly(
            Pubkey::find_program_address(&[vs::FACTORY_SEED], program).0,
            false,
        ));
        rest.push(AccountMeta::new_readonly(
            vs::vault_address_of(program, 7).0,
            false,
        ));
    }
    // As the services make each driver's group.
    for driver in &drivers {
        let authority = shonin_station::venue_authority(&v.station, &driver.venue).0;
        rest.extend([
            AccountMeta::new_readonly(driver.program, false),
            AccountMeta::new_readonly(
                Pubkey::find_program_address(
                    &[driver.program.as_ref()],
                    &anchor_lang::solana_program::bpf_loader_upgradeable::ID,
                )
                .0,
                false,
            ),
            AccountMeta::new_readonly(
                registry::approval_address(ApprovalKind::Driver, &driver.name),
                false,
            ),
            AccountMeta::new_readonly(
                registry::revoked_address(ApprovalKind::Driver, &driver.name, &driver.program),
                false,
            ),
            AccountMeta::new_readonly(authority, false),
            AccountMeta::new(dollars(&authority, &v), false),
        ]);
    }
    let expected = anchor_with(
        shonin_vault::ID,
        shonin_vault::accounts::Activate {
            author: v.author,
            factory: vs::factory_address(),
            strategy: shonin_publishing::state::strategy_address(7),
            review: Pubkey::find_program_address(
                &[shonin_publishing::state::REVIEW_SEED, &7u64.to_le_bytes()],
                &shonin_publishing::ID,
            )
            .0,
            vault: v.vault,
            restricted: vs::restricted_address(&v.vault).0,
            author_holder: vs::holder_address(&v.vault, &v.author).0,
            platform_holder: vs::holder_address(&v.vault, &v.platform).0,
            vault_dollars: dollars(&v.vault, &v),
            author_dollars: dollars(&v.author, &v),
            mint: v.mint,
            token_program: TOKEN,
            associated_token_program: anchor_spl::associated_token::ID,
            system_program: SYSTEM,
            station_program: shonin_station::ID,
            station_config: shonin_station::config_address(),
            station: v.station,
            station_dollars: dollars(&v.station, &v),
        },
        &rest,
        shonin_vault::instruction::Activate {
            args: shonin_vault::ActivateArgs {
                strategy_id: 7,
                allowances: theirs,
                idle_floor_bps: 1_000,
                tank_funding: 20_000_000,
                tank_keep: 5_000_000,
                statement: [8; 32],
                terms: [9; 32],
                driver_accounts: vec![6, 6],
                previous: 2,
            },
        },
    );
    assert_eq!(super::vault::activate(&v, &activation), expected);
}

#[test]
fn paying_to_publish_is_publishing_s_submit() {
    let (author, mint, platform) = (key(), key(), key());
    let token_2022 = anchor_spl::token_2022::ID;
    for token_program in [TOKEN, token_2022] {
        let ata = |owner: &Pubkey| {
            anchor_spl::associated_token::get_associated_token_address_with_program_id(
                owner,
                &mint,
                &token_program,
            )
        };
        let expected = anchor(
            shonin_publishing::ID,
            shonin_publishing::accounts::Submit {
                author,
                config: address::publishing_config(),
                submission: Pubkey::find_program_address(
                    &[
                        shonin_publishing::state::SUBMISSION_SEED,
                        &5u64.to_le_bytes(),
                    ],
                    &shonin_publishing::ID,
                )
                .0,
                mint,
                author_token: ata(&author),
                platform_token: ata(&platform),
                token_program,
                associated_token_program: anchor_spl::associated_token::ID,
                system_program: SYSTEM,
            },
            shonin_publishing::instruction::Submit { upload: [7; 32] },
        );
        assert_eq!(
            publishing::submit(&author, [7; 32], 5, &mint, &token_program, &platform),
            expected
        );
    }
}

// The stand-in venue's driver group ---------------------------------------------------------

#[test]
fn the_stand_in_driver_s_group_is_the_one_the_services_pass() {
    let mint = key();
    let station = key();
    let venue = shonin_local_venue::ID;
    let authority = shonin_station::venue_authority(&station, &venue).0;
    let pinned = Pinned {
        program: shonin_local_driver::ID,
        venue,
        authority,
        dollars: get_associated_token_address(&authority, &mint),
    };
    let held = [key(), key()];
    let mut expected = vec![
        AccountMeta::new_readonly(pinned.program, false),
        AccountMeta::new_readonly(authority, false),
        AccountMeta::new(pinned.dollars, false),
        AccountMeta::new_readonly(venue, false),
        AccountMeta::new_readonly(shonin_local_venue::venue_address_of(&venue), false),
        AccountMeta::new(shonin_local_venue::vault_address_of(&venue), false),
        AccountMeta::new_readonly(mint, false),
        AccountMeta::new_readonly(TOKEN, false),
        AccountMeta::new(
            shonin_local_venue::account_address_of(&venue, &authority),
            false,
        ),
    ];
    expected.extend(held.iter().map(|m| AccountMeta::new_readonly(*m, false)));
    assert_eq!(
        super::vault::stand_in_group(&pinned, &mint, &held),
        expected
    );
}

// Around the platform's instructions ----------------------------------------------------------

#[test]
fn the_compute_budget_is_the_sdk_s_and_asks_the_heap_only_for_the_vault_or_station() {
    use solana_compute_budget_interface::ComputeBudgetInstruction;
    assert_eq!(
        budget::compute_unit_limit(budget::COMPUTE_UNITS),
        ComputeBudgetInstruction::set_compute_unit_limit(1_400_000)
    );
    assert_eq!(
        budget::heap_frame(budget::HEAP),
        ComputeBudgetInstruction::request_heap_frame(shonin_station::heap::MAX_HEAP as u32)
    );
    let v = a_vault();
    let deposit = super::vault::request_deposit(&v, &key(), &key(), 1, None, 0);
    assert_eq!(
        budget::with_budget(vec![deposit.clone()]),
        vec![
            budget::compute_unit_limit(budget::COMPUTE_UNITS),
            budget::heap_frame(budget::HEAP),
            deposit
        ]
    );
    let submit = publishing::submit(&key(), [0; 32], 1, &key(), &TOKEN, &key());
    assert_eq!(
        budget::with_budget(vec![submit.clone()]),
        vec![budget::compute_unit_limit(budget::COMPUTE_UNITS), submit]
    );
}

#[test]
fn opening_a_dollar_account_is_the_associated_token_program_s_create_idempotent() {
    let (payer, owner, mint) = (key(), key(), key());
    assert_eq!(
        budget::open_dollars(&payer, &owner, &mint),
        anchor_spl::associated_token::spl_associated_token_account::instruction::create_associated_token_account_idempotent(
            &payer, &owner, &mint, &TOKEN
        )
    );
}

// Reading ---------------------------------------------------------------------------------------

#[test]
fn the_restricted_list_reads_and_hashes_as_the_vault_does() {
    use shonin_vault::codes;
    let list = codes::merged(&[codes::code("CA")], [[codes::code("GB")].as_slice()]);
    let account = vs::Restricted {
        vault: key(),
        bump: 254,
        codes: list.clone(),
    };
    let mut data = Vec::new();
    account.try_serialize(&mut data).unwrap();
    assert_eq!(read::restricted_codes(&data), Some(list.clone()));
    assert_eq!(read::restricted_hash(&list), codes::hash(&list));
    assert_eq!(read::restricted_hash(&[]), codes::hash(&[]));
    let text: Vec<String> = list.iter().map(read::code_text).collect();
    assert_eq!(text.join(","), codes::text(&list));
    // Room past the list, as an account grown for a longer one has, is not
    // read as codes.
    data.extend([0u8; 64]);
    assert_eq!(read::restricted_codes(&data), Some(list));
    assert_eq!(read::restricted_codes(&data[..50]), None);
    data[0] ^= 1;
    assert_eq!(read::restricted_codes(&data), None, "another account");
}

#[test]
fn the_factory_reads_and_its_terms_hash_as_the_author_s_activation_states_them() {
    let mut terms = vs::Terms::default();
    terms.fees.holding_bps = 200;
    terms.fees.author_bps = 2_000;
    terms.fees.platform_bps = 500;
    terms.fees.transaction_bps = 10;
    terms.exit_band_bps = 300;
    terms.minimum_deposit = 10_000_000;
    terms.minimum_tank = 25_000_000;
    terms.dead_assets = 1_000;
    terms.timers.slot_interval = 3_600;
    terms.timers.wind_down = 86_400;
    let factory = vs::Factory {
        admin: key(),
        platform: key(),
        mint: key(),
        token_program: TOKEN,
        cluster: shonin_common::digest::cluster_value("devnet"),
        chain_name: [1; 32],
        registry: shonin_registry::ID,
        publishing: shonin_publishing::ID,
        prices: shonin_prices::ID,
        station_program: shonin_station::ID,
        previous: key(),
        terms,
        vault_count: 3,
        bump: 255,
    };
    let mut data = Vec::new();
    factory.try_serialize(&mut data).unwrap();
    let read = read::Factory::read(&data).expect("the factory reads");
    assert_eq!(read.admin, factory.admin);
    assert_eq!(read.platform, factory.platform);
    assert_eq!(read.mint, factory.mint);
    assert_eq!(read.token_program, TOKEN);
    assert_eq!(read.cluster, factory.cluster);
    assert_eq!(read.previous, factory.previous);
    assert_eq!(read.minimum_deposit, 10_000_000);
    assert_eq!(read.minimum_tank, 25_000_000);
    assert_eq!(read.terms_hash(), shonin_vault::factory::terms_hash(&terms));
    let mut other = data.clone();
    other[..8].copy_from_slice(vs::Restricted::DISCRIMINATOR);
    assert_eq!(read::Factory::read(&other), None, "another account");
    assert_eq!(read::Factory::read(&data[..400]), None, "too short");
}

#[test]
fn a_cluster_and_a_purpose_hash_as_the_programs_hash_them() {
    for name in ["localnet", "devnet", "mainnet"] {
        assert_eq!(
            read::cluster_value(name),
            shonin_common::digest::cluster_value(name)
        );
    }
    assert_eq!(
        read::purpose("shonin-usage"),
        shonin_common::digest::purpose("shonin-usage")
    );
}

// The transaction ---------------------------------------------------------------------------------

fn some_instructions() -> (Pubkey, Vec<Instruction>) {
    let v = a_vault();
    let payer = key();
    let ixs = budget::with_budget(vec![
        budget::open_dollars(&payer, &payer, &v.mint),
        super::vault::request_deposit(&v, &payer, &payer, 10_000_000, Some([1; 32]), 0),
    ]);
    (payer, ixs)
}

#[test]
fn a_legacy_transaction_on_the_wire_is_the_sdk_s_with_its_signatures() {
    use solana_transaction::Transaction;
    let (payer, ixs) = some_instructions();
    let blockhash = Hash::new_from_array([4; 32]);
    let unsigned = Unsigned::legacy(&payer, &ixs, blockhash).unwrap();
    let message = solana_message::Message::new_with_blockhash(&ixs, Some(&payer), &blockhash);
    let mut tx = Transaction::new_unsigned(message);
    assert_eq!(unsigned.signers(), &[payer]);
    assert_eq!(unsigned.message(), tx.message_data().as_slice());
    assert_eq!(unsigned.wire(), bincode::serialize(&tx).unwrap());
    tx.signatures = vec![[7u8; 64].into()];
    assert_eq!(
        unsigned.signed(&[[7; 64]]).unwrap(),
        bincode::serialize(&tx).unwrap()
    );
    assert_eq!(
        unsigned.signed(&[]),
        Err(TransactionError::Signatures {
            needed: 1,
            given: 0
        })
    );
}

#[test]
fn a_version_0_transaction_on_the_wire_is_the_sdk_s_through_its_table() {
    use solana_message::{VersionedMessage, v0};
    use solana_transaction::versioned::VersionedTransaction;
    let (payer, ixs) = some_instructions();
    let blockhash = Hash::new_from_array([5; 32]);
    let v = a_vault();
    let table = AddressLookupTableAccount {
        key: key(),
        addresses: vec![
            v.vault,
            v.vault_dollars,
            v.mint,
            address::dollars(&payer, &v.mint),
        ],
    };
    let unsigned = Unsigned::v0(&payer, &ixs, std::slice::from_ref(&table), blockhash).unwrap();
    let message = v0::Message::try_compile(&payer, &ixs, &[table], blockhash).unwrap();
    assert!(
        !message.address_table_lookups.is_empty(),
        "the table is used"
    );
    let tx = VersionedTransaction {
        signatures: vec![[2u8; 64].into()],
        message: VersionedMessage::V0(message),
    };
    assert_eq!(
        unsigned.signed(&[[2; 64]]).unwrap(),
        bincode::serialize(&tx).unwrap()
    );
    assert_eq!(unsigned.message(), tx.message.serialize().as_slice());
}

#[test]
fn a_transaction_too_large_for_the_wire_is_refused_with_its_size() {
    let v = a_vault();
    let payer = key();
    let ixs: Vec<Instruction> = (0..12)
        .map(|n| super::vault::request_deposit(&v, &payer, &key(), 1, Some([1; 32]), n))
        .collect();
    match Unsigned::legacy(&payer, &ixs, Hash::default()) {
        Err(TransactionError::TooLarge { size }) => assert!(size > 1232),
        other => panic!("expected too large, got {other:?}"),
    }
}

#[test]
fn a_length_on_the_wire_takes_seven_bits_a_byte() {
    // Checked through the signature count, which a transaction writes so.
    let one_signer = Unsigned::legacy(&key(), &[], Hash::default()).unwrap();
    assert_eq!(one_signer.wire()[0], 1);
    let mut out = Vec::new();
    super::transaction::short_vec(300, &mut out);
    assert_eq!(out, [0xac, 0x02]);
}

// The services' own builders, where they build the same instruction --------------------------------

#[cfg(feature = "solana")]
mod services {
    use super::*;
    use crate::solana::platform::{Platform, VaultAccounts};

    fn platform(v: &VaultKeys) -> Platform {
        Platform {
            cluster_name: "localnet".into(),
            cluster: [0; 32],
            mint: v.mint,
            admin: key(),
            platform: v.platform,
            operator: key(),
            checker: None,
        }
    }

    fn accounts(p: &Platform, v: &VaultKeys) -> VaultAccounts {
        p.vault_accounts(v.strategy_id)
    }

    #[test]
    fn the_depositor_s_builders_make_what_the_services_make() {
        let v = a_vault();
        let p = platform(&v);
        let (payer, receiver) = (key(), key());
        assert_eq!(
            super::super::vault::request_deposit(&v, &payer, &receiver, 9, Some([3; 32]), 2),
            p.ix_request_deposit(&v.vault, &payer, &receiver, 9, Some([3; 32]), 2)
        );
        assert_eq!(
            super::super::vault::request_redeem(&v, &payer, 9, &receiver, 2),
            p.ix_request_redeem(&v.vault, &payer, 9, &receiver, 2)
        );
        assert_eq!(
            super::super::vault::redeem_closed(&v, &payer, 9, &receiver),
            p.ix_redeem_closed(&accounts(&p, &v), &payer, 9, &receiver)
        );
        assert_eq!(
            super::super::vault::collect_held(&v, &payer, Held::Fees, None),
            p.ix_collect_held(
                &accounts(&p, &v),
                &payer,
                shonin_vault::upkeep::HeldIn::Fees,
                None,
                &dollars(&payer, &v)
            )
        );
    }

    #[test]
    fn the_author_s_builders_make_what_the_services_make() {
        let v = a_vault();
        let p = platform(&v);
        assert_eq!(
            super::super::vault::fund_tank(&v, &v.author, 7),
            p.ix_fund_tank(&v.author, &v.vault, 7)
        );
        assert_eq!(
            super::super::vault::set_tank_keep(&v, 7),
            p.ix_set_tank_keep(&v.author, &v.vault, 7)
        );
        assert_eq!(
            super::super::vault::retire(&v),
            p.ix_retire(&v.author, &v.vault)
        );
    }
}
