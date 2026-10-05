//! What Solana adds: every account a vault instruction reads or writes is
//! checked to be the one it stands for; nothing can be created twice, revived
//! after it is closed, or created at another's address; whoever creates an
//! account pays its rent and is refunded it when it is closed, to the address
//! the account names. Shares are balances in a per-holder account and never
//! move between holders: there is no transfer. The counterpart of
//! `Vault.t.sol`'s `test_shares_never_move_between_holders` and of the
//! Solana-only checks the station's own `station_accounts` makes.

use anchor_lang::{AccountSerialize, InstructionData, ToAccountMetas};
use shonin_solana_tests::station::*;
use shonin_solana_tests::vault::*;
use shonin_solana_tests::*;
use solana_keypair::Keypair;
use solana_signer::Signer;

/// Swaps every `from` in `ix`'s accounts for `to`.
fn swap(
    mut ix: solana_instruction::Instruction,
    from: Pubkey,
    to: Pubkey,
) -> solana_instruction::Instruction {
    for meta in ix.accounts.iter_mut() {
        if meta.pubkey == from {
            meta.pubkey = to;
        }
    }
    ix
}

#[test]
fn shares_never_move_between_holders() {
    // The program has no instruction that moves a holder's shares to another
    // holder: the only ways shares leave a holding are a redemption, a claim,
    // the fallback, a tank fill and the closed vault's door.
    let idl = std::fs::read_to_string(format!(
        "{}/../target/idl/shonin_vault.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .expect("the IDL is built");
    for word in [
        "\"transfer\"",
        "\"approve\"",
        "\"transfer_from\"",
        "\"transfer_shares\"",
    ] {
        assert!(!idl.contains(&format!("\"name\": {word}")), "{word} exists");
    }
    // And a holder's own redemption can't take someone else's shares.
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let a = env.deposit_as(&v, &alice, 1_000_000_000);
    env.deposit_as(&v, &bob, 1_000_000_000);
    let ix = env.ix_request_redeem(&v, &bob.pubkey(), a, &bob.pubkey());
    let ix = swap(
        ix,
        holder_address(&v.vault, &bob.pubkey()),
        holder_address(&v.vault, &alice.pubkey()),
    );
    expect_failure(env.send(vec![ix], &[&bob]));
    assert_eq!(env.shares(&v, &alice.pubkey()), a);
}

#[test]
fn every_signer_is_the_one_its_power_belongs_to() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    env.deposit_as(&v, &alice, 1_000_000_000);
    // The author's powers.
    for data in [
        shonin_vault::instruction::Retire {}.data(),
        shonin_vault::instruction::SetTankKeep { assets: 1 }.data(),
        shonin_vault::instruction::SetIdleFloor { bps: 0 }.data(),
        shonin_vault::instruction::RequestTankWithdrawal {}.data(),
    ] {
        let ix = swap(env.ix_author(&v, data), env.author.pubkey(), bob.pubkey());
        expect_failure(env.send(vec![ix], &[&bob]));
    }
    let ix = swap(
        env.ix_withdraw_tank_unsigned(&v, 1),
        env.author.pubkey(),
        bob.pubkey(),
    );
    expect_failure(env.send(vec![ix], &[&bob]));
    // The admin's.
    let ix = env.ix_admin(&v, &bob.pubkey(), shonin_vault::instruction::Kill {}.data());
    expect_failure(env.send(vec![ix], &[&bob]));
    // A holder's: collecting someone else's units or redeeming their shares
    // from a closed vault.
    let ix = swap(
        env.ix_collect_units(&v, &bob.pubkey()),
        holder_address(&v.vault, &bob.pubkey()),
        holder_address(&v.vault, &alice.pubkey()),
    );
    expect_failure(env.send(vec![ix], &[&bob]));
    // An instruction that needs a signature is refused without it.
    let mut ix = env.ix_request_redeem(&v, &alice.pubkey(), 1, &alice.pubkey());
    ix.accounts[0].is_signer = false;
    let payer = env.payer.insecure_clone();
    expect_failure(env.send(vec![ix], &[&payer]));
}

#[test]
fn a_look_alike_vault_or_holding_owned_by_another_program_is_refused() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    env.deposit_as(&v, &alice, 1_000_000_000);
    // A copy of the vault's account, owned by another program.
    let fake = Pubkey::new_unique();
    let mut account = env.svm.get_account(&v.vault).unwrap();
    account.owner = shonin_test_vault::ID;
    env.svm.set_account(fake, account).unwrap();
    let ix = swap(env.ix_record_valuation(&v), v.vault, fake);
    env.to_next_slot();
    let payer = env.payer.insecure_clone();
    expect_code(
        env.send_fitting(vec![ix], &[&payer]),
        anchor_error(anchor_lang::error::ErrorCode::AccountOwnedByWrongProgram),
    );
    // A copy of alice's holding with a million times her shares, owned by
    // the system program at another address.
    let mut holder = env.holder(&v, &alice.pubkey()).unwrap();
    holder.shares *= 1_000_000;
    let mut data = Vec::new();
    holder.try_serialize(&mut data).unwrap();
    let forged = Pubkey::new_unique();
    let lamports = env.svm.minimum_balance_for_rent_exemption(data.len());
    env.svm
        .set_account(
            forged,
            solana_account::Account {
                lamports,
                data,
                owner: shonin_vault::ID,
                executable: false,
                rent_epoch: 0,
            },
        )
        .unwrap();
    // Even owned by this program, an account at an address other than the
    // holding's own is refused where the address is checked.
    let ix = swap(
        env.ix_request_redeem(&v, &alice.pubkey(), holder.shares, &alice.pubkey()),
        holder_address(&v.vault, &alice.pubkey()),
        forged,
    );
    expect_code(
        env.send(vec![ix], &[&alice]),
        anchor_error(anchor_lang::error::ErrorCode::ConstraintSeeds),
    );
    assert_eq!(
        env.holder(&v, &alice.pubkey()).unwrap().shares * 1_000_000,
        holder.shares
    );
}

#[test]
fn the_mint_the_token_program_and_the_vaults_dollar_account_are_its_own() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    env.give(&alice.pubkey(), 1_000_000_000);
    let hash = env.vault(&v).restricted_hash;
    let ix = env.ix_request_deposit(
        &v,
        &alice.pubkey(),
        &alice.pubkey(),
        1_000_000_000,
        Some(hash),
    );
    // Another mint.
    let other_mint = Pubkey::new_unique();
    expect_failure(env.send(vec![swap(ix.clone(), env.mint, other_mint)], &[&alice]));
    // Token-2022 standing in for the classic token program.
    expect_failure(env.send(
        vec![swap(
            ix.clone(),
            anchor_spl::token::ID,
            anchor_spl::token_2022::ID,
        )],
        &[&alice],
    ));
    // Alice's own account standing in for the vault's.
    let alices = ata(&alice.pubkey(), &env.mint);
    expect_failure(env.send(
        vec![swap(ix.clone(), v.home.vault_dollars, alices)],
        &[&alice],
    ));
    expect_ok(env.send(vec![ix], &[&alice]));
}

#[test]
fn requests_and_claims_are_read_only_at_their_own_addresses() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let a = env.deposit_as(&v, &alice, 1_000_000_000);
    env.deposit_as(&v, &bob, 1_000_000_000);
    let first = env.redeem(&v, &alice, a);
    let second = env.redeem(&v, &bob, 1);
    env.skip(DAY + SLOT);
    // Alice's fallback naming bob's request.
    let ix = swap(
        env.ix_take_pro_rata(&v, &alice.pubkey(), first),
        env.request_key(&v, first),
        env.request_key(&v, second),
    );
    expect_code(
        env.send_fitting(vec![ix], &[&alice]),
        vault_error(VaultError::WrongAccount),
    );
    // A request made at an address that isn't its maker's: alice's.
    let mut ix = env.ix_request_redeem(&v, &bob.pubkey(), 1, &bob.pubkey());
    let bobs = ix.accounts[3].pubkey;
    ix = swap(ix, bobs, request_address(&v.vault, &alice.pubkey(), 1));
    expect_code(
        env.send(vec![ix], &[&bob]),
        vault_error(VaultError::WrongAccount),
    );
    // A claim booked at another claim's address.
    let ix = swap(
        env.ix_claim_instead(&v, &alice.pubkey(), first),
        claim_address(&v.vault, first),
        claim_address(&v.vault, second),
    );
    expect_code(
        env.send(vec![ix], &[&alice]),
        vault_error(VaultError::WrongAccount),
    );
}

#[test]
fn no_two_records_share_an_address() {
    let vault = Pubkey::new_unique();
    let mut seen = std::collections::HashSet::new();
    let owners: Vec<Pubkey> = (0..50).map(|_| Pubkey::new_unique()).collect();
    for id in 0..200u64 {
        assert!(seen.insert(claim_address(&vault, id)));
        assert!(seen.insert(shonin_vault::state::vault_address(id).0));
        // The vault's own requests (its tank's fills), and a claim's.
        assert!(seen.insert(request_address(&vault, &vault, id)));
        assert!(seen.insert(request_address(&vault, &claim_address(&vault, 1), id)));
    }
    for owner in &owners {
        assert!(seen.insert(holder_address(&vault, owner)));
        for nonce in 0..20u64 {
            assert!(seen.insert(request_address(&vault, owner, nonce)));
        }
    }
    assert!(seen.insert(shonin_vault::state::restricted_address(&vault).0));
    assert!(seen.insert(factory_address()));
    // Each kind has its own prefix, and a request's seeds name its maker, so
    // no owner or nonce can be chosen to land on another record.
}

#[test]
fn nothing_is_made_twice_and_a_closed_record_stays_closed() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    let shares = env.deposit_as(&v, &alice, 1_000_000_000);
    // Recording a statement again keeps her shares.
    let hash = env.vault(&v).restricted_hash;
    let ix = env.ix_record_statement(&v, &alice.pubkey(), hash);
    expect_ok(env.send(vec![ix], &[&alice]));
    assert_eq!(env.shares(&v, &alice.pubkey()), shares);
    // A done request is closed, its rent to its payer.
    let id = 1;
    assert!(env.request(&v, id).unwrap().done);
    let payer = env.request(&v, id).unwrap().payer;
    assert_eq!(payer, alice.pubkey());
    let rent = env.lamports(&env.request_key(&v, id));
    let before = env.lamports(&payer);
    // To anyone but its payer, refused.
    let bob = env.bob.insecure_clone();
    let ix = swap(env.ix_close_request(&v, id), payer, bob.pubkey());
    expect_code(
        env.send(vec![ix], &[&bob]),
        vault_error(VaultError::WrongAccount),
    );
    // Closed, and in the same transaction sent lamports again by a
    // stranger: it stays an empty account of the system program's.
    let sender = env.payer.insecure_clone();
    let address = env.request_key(&v, id);
    let close = env.ix_close_request(&v, id);
    let revive =
        anchor_lang::solana_program::system_instruction::transfer(&sender.pubkey(), &address, rent);
    expect_ok(env.send(vec![close, revive], &[&sender]));
    assert_eq!(env.lamports(&payer), before + rent);
    let account = env.svm.get_account(&address).unwrap();
    assert_eq!(account.owner, solana_sdk_ids::system_program::ID);
    assert!(account.data.is_empty());
    assert!(env.request(&v, id).is_none());
    // Closing it again is nothing; settling past it reads it as done.
    let ix = env.ix_close_request(&v, id);
    expect_code(
        env.send(vec![ix], &[&sender]),
        vault_error(VaultError::Nothing),
    );
    // A request not yet done can't be closed.
    let open = env.redeem(&v, &alice, 1);
    let ix = env.ix_close_request(&v, open);
    expect_code(
        env.send(vec![ix], &[&sender]),
        vault_error(VaultError::NotAllowed),
    );
    env.price(&v);
}

#[test]
fn a_claim_is_paid_for_by_its_requests_rent_and_refunds_its_payer() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let a = env.deposit_as(&v, &alice, 100_000_000_000);
    env.deposit_as(&v, &bob, 100_000_000_000);
    env.deploy_spot(&v, 190_000_000_000);
    let before = env.lamports(&alice.pubkey());
    let id = env.redeem(&v, &alice, a);
    let rent = env.lamports(&env.request_key(&v, id));
    assert!(env.lamports(&alice.pubkey()) < before);
    env.price(&v);
    // The request became the claim; whoever settled paid nothing for it.
    assert!(env.request(&v, id).is_none());
    assert_eq!(env.lamports(&claim_address(&v.vault, id)), rent);
    assert_eq!(env.claim(&v, id).unwrap().payer, alice.pubkey());
    // Not closed before it is paid.
    let payer = env.payer.insecure_clone();
    let ix = env.ix_close_claim(&v, id);
    expect_code(
        env.send(vec![ix], &[&payer]),
        vault_error(VaultError::NotAllowed),
    );
    expect_ok(env.settle_claim(&v, id));
    let before = env.lamports(&alice.pubkey());
    let ix = env.ix_close_claim(&v, id);
    expect_ok(env.send(vec![ix], &[&payer]));
    assert_eq!(env.lamports(&alice.pubkey()), before + rent);
}

#[test]
fn an_empty_holding_is_closed_by_its_owner_and_made_again_from_nothing() {
    use shonin_vault::state::Holder;
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    let shares = env.deposit_as(&v, &alice, 1_000_000_000);
    let close = |owner: &Pubkey, payer: Pubkey| solana_instruction::Instruction {
        program_id: shonin_vault::ID,
        accounts: shonin_vault::accounts::CloseHolder {
            owner: *owner,
            vault: v.vault,
            holder: holder_address(&v.vault, owner),
            payer,
        }
        .to_account_metas(None),
        data: shonin_vault::instruction::CloseHolder {}.data(),
    };
    expect_code(
        env.send(vec![close(&alice.pubkey(), alice.pubkey())], &[&alice]),
        vault_error(VaultError::NotAllowed),
    );
    env.redeem_as(&v, &alice, shares);
    let before = env.lamports(&alice.pubkey());
    let rent = env.lamports(&holder_address(&v.vault, &alice.pubkey()));
    expect_ok(env.send(vec![close(&alice.pubkey(), alice.pubkey())], &[&alice]));
    assert_eq!(env.lamports(&alice.pubkey()), before + rent - 5_000);
    assert!(env.holder(&v, &alice.pubkey()).is_none());
    // Made again, it holds nothing and no statement.
    let hash = env.vault(&v).restricted_hash;
    let ix = env.ix_record_statement(&v, &alice.pubkey(), hash);
    expect_ok(env.send(vec![ix], &[&alice]));
    let fresh: Holder = env.holder(&v, &alice.pubkey()).unwrap();
    assert_eq!(fresh.shares, 0);
    // The author's and the platform's holdings stay.
    let author = env.author.insecure_clone();
    expect_code(
        env.send(vec![close(&author.pubkey(), author.pubkey())], &[&author]),
        vault_error(VaultError::NotAllowed),
    );
}

#[test]
fn a_vaults_rent_is_its_authors_and_a_requests_its_makers() {
    let mut env = Env::with_vaults();
    let author = env.author.pubkey();
    let before = env.lamports(&author);
    let v = env.open(&[Declared::local()]);
    let spent = before - env.lamports(&author);
    // The vault, its list, the two fee holdings, its dollar account, its
    // station and the station's dollar accounts: rent the author paid.
    assert!(spent > 0);
    let alice = env.alice.insecure_clone();
    let payer = env.payer.insecure_clone();
    env.deposit(&v, &alice, 1_000_000_000);
    env.to_next_slot();
    let before = env.lamports(&payer.pubkey());
    expect_ok(env.value_vault(&v));
    env.settle_all(&v);
    // Valuing and settling paid only transaction fees: no account was
    // created at their sender's cost.
    assert!(before - env.lamports(&payer.pubkey()) <= 2 * 15_000);
}

#[test]
fn a_keepers_fill_request_is_rented_by_the_keeper_and_refunded_to_it() {
    let (mut env, v) = Env::with_vault();
    let author = env.author.insecure_clone();
    env.deposit_as(&v, &author, 1_000_000_000);
    let ix = env.ix_author(
        &v,
        shonin_vault::instruction::SetTankKeep {
            assets: MINIMUM_TANK * 2,
        }
        .data(),
    );
    expect_ok(env.as_author(ix));
    let keeper = Keypair::new();
    env.svm.airdrop(&keeper.pubkey(), 1_000_000_000).unwrap();
    let ix = env.ix_keep(&v, &keeper.pubkey(), None);
    expect_ok(env.send(vec![ix], &[&keeper]));
    let id = env.vault(&v).request_count;
    assert_eq!(env.request(&v, id).unwrap().payer, keeper.pubkey());
    env.price(&v);
    let before = env.lamports(&keeper.pubkey());
    let rent = env.lamports(&env.request_key(&v, id));
    let ix = env.ix_close_request(&v, id);
    let payer = env.payer.insecure_clone();
    expect_ok(env.send(vec![ix], &[&payer]));
    assert_eq!(env.lamports(&keeper.pubkey()), before + rent);
}

#[test]
fn the_authors_own_deposit_fallback_and_serve_land_in_their_one_holding() {
    // The author's holding is also the pool's copy fees are paid into: each
    // change to it, whichever instruction makes it, must survive the other.
    let (mut env, v) = Env::with_vault();
    let author = env.author.insecure_clone();
    let alice = env.alice.insecure_clone();
    let shares = env.deposit_as(&v, &author, 50_000_000_000);
    assert!(shares > 0, "the author's settled deposit was lost");
    let vault = env.vault(&v);
    let holders: u128 = [author.pubkey(), env.platform.pubkey()]
        .iter()
        .map(|who| env.shares(&v, who))
        .sum();
    assert_eq!(
        vault.total_supply,
        holders + vault.dead_shares + vault.pending_shares + vault.unpaid_holding
    );
    env.deposit_as(&v, &alice, 50_000_000_000);
    env.deploy_spot(&v, 80_000_000_000);
    env.gain(&v, 10_000_000_000);
    env.price(&v);
    // The author leaves through the fallback: a fee slice to themself and
    // units, on one holding.
    let id = env.redeem(&v, &author, shares);
    env.skip(DAY + SLOT);
    expect_ok(env.take_pro_rata(&v, &author, id));
    let holding = env.holder(&v, &author.pubkey()).unwrap();
    assert!(holding.units > 0, "the author's units were lost");
    assert!(holding.shares > 0, "the author's fee slice was lost");
    expect_ok(env.serve(&v, &author.pubkey()));
    let holding = env.holder(&v, &author.pubkey()).unwrap();
    assert_eq!(holding.units, 0);
    assert!(
        holding.unit_credit > 0,
        "what came home for the author was lost"
    );
    let ix = env.ix_collect_units(&v, &author.pubkey());
    expect_ok(env.send(vec![ix], &[&author]));
}
