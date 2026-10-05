//! The call depth a vault's unwind needs, measured first, since everything
//! else is built on it: the vault calls the station, the station calls the
//! venue, the venue calls the token program. That is four levels of the five
//! the runtime allows (the transaction's own instruction and four nested
//! calls), so there is exactly one to spare, and only while nothing else
//! nests: one more level in between still works, two fail. The station's own
//! move of what came home from its venue authority's dollar account to its
//! own runs a level shallower, under the vault and the station. A driver is only
//! ever asked (vault, station, driver: three levels), never in the path of a
//! venue call.

use shonin_solana_tests::station::*;
use shonin_solana_tests::*;
use solana_instruction::error::InstructionError;
use solana_signer::Signer;
use solana_transaction_error::TransactionError;

/// A vault whose station holds a spot position at the venue, with idle
/// dollars at the venue to sweep.
fn holding() -> (Env, Home) {
    let (mut env, home) = Env::with_station();
    expect_ok(env.pull(&home, FUNDS));
    env.approve_executor(&home);
    env.fund_venue(&home, FUNDS);
    env.place(&home, vec![order(spot(), true, UNIT, PRICE)]);
    (env, home)
}

fn unwind_at_depth(env: &mut Env, home: &Home, depth: u8) -> TxResult {
    let ix = env.ix_unwind(home, &home.vault, WAD / 4, false);
    let call = env.ix_call(home, ix, depth);
    let payer = env.payer.insecure_clone();
    env.send(vec![call], &[&payer])
}

fn too_deep(result: &TxResult) -> bool {
    matches!(
        result,
        Err(failed) if matches!(
            failed.err,
            TransactionError::InstructionError(_, InstructionError::CallDepth)
        )
    )
}

#[test]
fn a_vaults_unwind_reaches_the_token_program_four_levels_down() {
    let (mut env, home) = holding();
    let before = env.usd_balance(&home.vault);
    let meta = expect_ok(unwind_at_depth(&mut env, &home, 0));
    // A quarter of the position sold and swept, a quarter of everything home.
    assert_eq!(
        env.position(&home.authority(0), spot()),
        (UNIT * 3) as i128 / 4
    );
    assert_eq!(env.usd_balance(&home.vault), before + FUNDS / 4);
    // The venue's withdrawal moved the dollars: the token program ran at the
    // fourth level, under the vault, the station and the venue.
    let depths: Vec<&String> = meta
        .logs
        .iter()
        .filter(|line| line.contains(" invoke [4]"))
        .collect();
    assert!(
        depths
            .iter()
            .any(|line| line.contains(&anchor_spl::token::ID.to_string())),
        "the token program never ran at depth four:\n{}",
        meta.logs.join("\n")
    );
    // The station moved what came home from its authority's dollar account
    // to its own at the third level.
    assert!(
        meta.logs.iter().any(|line| {
            line.contains(&anchor_spl::token::ID.to_string()) && line.contains(" invoke [3]")
        }),
        "the station's own transfer never ran at depth three:\n{}",
        meta.logs.join("\n")
    );
    assert!(
        !meta.logs.iter().any(|line| line.contains(" invoke [5]")),
        "something nested deeper than the venue's token call"
    );
}

#[test]
fn one_level_to_spare_and_no_more() {
    let (mut env, home) = holding();
    // One more call in between still fits: the token program runs at five.
    let meta = expect_ok(unwind_at_depth(&mut env, &home, 1));
    assert!(meta.logs.iter().any(|line| line.contains(" invoke [5]")));
    // Two more do not.
    let result = unwind_at_depth(&mut env, &home, 2);
    assert!(
        too_deep(&result),
        "expected the call to be too deep, got {:?}",
        result.as_ref().err().map(|f| &f.err)
    );
}

#[test]
fn a_driver_is_asked_at_the_third_level_and_calls_nothing() {
    let (mut env, home) = holding();
    let ix = env.ix_value(&home);
    let call = env.ix_call(&home, ix, 0);
    let payer = env.payer.insecure_clone();
    let meta = expect_ok(env.send(vec![call], &[&payer]));
    let driver = shonin_local_driver::ID.to_string();
    assert!(meta
        .logs
        .iter()
        .any(|line| line.contains(&driver) && line.contains(" invoke [3]")));
    assert!(!meta.logs.iter().any(|line| line.contains(" invoke [4]")));
    let value = Valuation::try_from_slice(&meta.return_data.data).unwrap();
    assert_eq!(value.mark, FUNDS);
    let _ = env.executor.pubkey();
}

use anchor_lang::AnchorDeserialize;
