//! The request a queued claim is priced by lives where the program made it,
//! not at the address its id would give.
//!
//! The vault program queues a claim the cash covers by making a request at
//! the claim's own request address for the nonce the claim's `request`
//! field held *before* (0 for a claim never queued), and then stores the new
//! request's queue id in that field. So for a claim reading `request = N`,
//! the request it is priced by sits at `["request", vault, claim, <previous
//! nonce>]` and holds id `N`; nothing is at `["request", vault, claim, N]`
//! until the claim is queued again. The program finds the request by its
//! id; so must the app and the keeper, and the builders name what they
//! found. (`settle_claim`, which makes the next request, is right to use `N`
//! as the nonce.) The vault program's own tests run these builders against
//! the program for both doors.

#![cfg(feature = "solana-ix")]

use chain::solana_ix::accounts::{Claim, ClaimState, Request, RequestKind};
use chain::solana_ix::vault::{ClaimKeys, VaultKeys, claim_closed, take_pro_rata_claim};
use chain::solana_ix::{Pubkey, address};

fn a_vault() -> VaultKeys {
    VaultKeys::new(
        4,
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        Pubkey::new_unique(),
    )
}

fn a_claim(id: u64, owner: Pubkey) -> Claim {
    Claim {
        id,
        owner,
        shares: 100,
        booked_at: 900,
        last_attempt_at: 950,
        state: ClaimState::Pending,
        paid: 0,
        request: 0,
        payer: owner,
        held: 0,
    }
}

fn a_request(vault: &Pubkey, id: u64, kind: RequestKind, reference: u64, owner: Pubkey) -> Request {
    Request {
        vault: *vault,
        id,
        owner,
        kind,
        earliest: 1_000,
        done: false,
        amount: 100,
        reference,
        payer: owner,
        held: 0,
        held_for: Pubkey::default(),
    }
}

/// The vault's requests as a node lists them: every one the vault has, with
/// its address.
type Requests = Vec<(Pubkey, Request)>;

/// What the program does when `settle_claim` queues `claim` as request
/// `next_id`: the request made at the claim's address for the nonce it
/// holds, then the claim holding the request's id.
fn queue(v: &VaultKeys, claim: &mut Claim, next_id: u64, requests: &mut Requests) -> Pubkey {
    let made = address::request(&v.vault, &address::claim(&v.vault, claim.id), claim.request);
    requests.push((
        made,
        a_request(&v.vault, next_id, RequestKind::Claim, claim.id, claim.owner),
    ));
    claim.state = ClaimState::Queued;
    claim.request = next_id;
    made
}

/// Where the app and the keeper look: among the vault's requests, the one
/// the claim is queued by.
fn found(v: &VaultKeys, claim: &Claim, requests: &Requests) -> Option<Pubkey> {
    requests
        .iter()
        .find(|(_, request)| claim.is_queued_by(&v.vault, request))
        .map(|(at, _)| *at)
}

/// Requests that share something with the one a claim is queued by: the same
/// id at another vault, the same id for another claim, the same id of
/// another kind, and another id at the address the claim's id would give.
fn decoys(v: &VaultKeys, claim: &Claim) -> Requests {
    let owner = claim.owner;
    vec![
        (
            Pubkey::new_unique(),
            a_request(
                &Pubkey::new_unique(),
                claim.request,
                RequestKind::Claim,
                claim.id,
                owner,
            ),
        ),
        (
            Pubkey::new_unique(),
            a_request(
                &v.vault,
                claim.request,
                RequestKind::Claim,
                claim.id + 1,
                owner,
            ),
        ),
        (
            Pubkey::new_unique(),
            a_request(
                &v.vault,
                claim.request,
                RequestKind::Redeem,
                claim.id,
                owner,
            ),
        ),
        (
            address::request(&v.vault, &address::claim(&v.vault, claim.id), claim.request),
            a_request(
                &v.vault,
                claim.request + 1,
                RequestKind::Claim,
                claim.id,
                owner,
            ),
        ),
    ]
}

#[test]
fn a_queued_claim_s_fallback_names_the_request_the_program_made() {
    let v = a_vault();
    let mut requests = Requests::new();
    let mut claim = a_claim(9, Pubkey::new_unique());
    // The vault has 12 requests; the cash covers the claim, and settling
    // queues it as request 13, made at nonce 0.
    let made = queue(&v, &mut claim, 13, &mut requests);
    assert_eq!(claim.request, 13);
    assert_ne!(
        made,
        address::request(&v.vault, &address::claim(&v.vault, 9), 13)
    );
    requests.extend(decoys(&v, &claim));
    let at = found(&v, &claim, &requests);
    assert_eq!(at, Some(made));

    // take_pro_rata_claim: owner, pool (8), claim, request, ...
    let ix = take_pro_rata_claim(&v, &claim.keys(), at.as_ref());
    assert_eq!(ix.accounts[10].pubkey, made);
    assert!(ix.accounts[10].is_writable);
}

#[test]
fn a_queued_claim_s_closed_vault_payout_names_the_request_the_program_made() {
    let v = a_vault();
    let mut requests = Requests::new();
    let mut claim = a_claim(21, Pubkey::new_unique());
    let made = queue(&v, &mut claim, 30, &mut requests);
    requests.extend(decoys(&v, &claim));

    // claim_closed: vault, station, claim, request (other), to, ...
    let ix = claim_closed(&v, &claim.keys(), found(&v, &claim, &requests).as_ref());
    assert_eq!(ix.accounts[3].pubkey, made);
}

#[test]
fn a_claim_queued_twice_is_named_by_its_second_request() {
    let v = a_vault();
    let mut requests = Requests::new();
    let mut claim = a_claim(5, Pubkey::new_unique());
    // Queued as request 8; the cash then no longer covers it at the
    // valuation, so it goes back to pending with `request = 8` (its first
    // request settled); tried again and queued as request 11, at the claim's
    // address for nonce 8.
    let first = queue(&v, &mut claim, 8, &mut requests);
    requests[0].1.done = true;
    claim.state = ClaimState::Pending;
    assert_eq!(found(&v, &claim, &requests), None, "a claim not queued");
    let second = queue(&v, &mut claim, 11, &mut requests);
    assert_ne!(first, second);
    assert_eq!(
        second,
        address::request(&v.vault, &address::claim(&v.vault, 5), 8)
    );
    assert_eq!(found(&v, &claim, &requests), Some(second));

    let ix = take_pro_rata_claim(&v, &claim.keys(), Some(&second));
    assert_eq!(ix.accounts[10].pubkey, second);
}

#[test]
fn a_claim_not_queued_names_no_request_and_its_next_is_made_at_the_id_it_holds() {
    use chain::solana_ix::vault::{Groups, settle_claim};
    let v = a_vault();
    let claim = ClaimKeys {
        id: 7,
        owner: Pubkey::new_unique(),
        request: 8,
    };
    let fallback = take_pro_rata_claim(&v, &claim, None);
    assert_eq!(fallback.accounts[10].pubkey, chain::solana_ix::ids::VAULT);
    assert!(!fallback.accounts[10].is_writable);
    // settle_claim makes the next request, at the nonce the claim holds.
    let caller = Pubkey::new_unique();
    let settle = settle_claim(&v, &caller, &claim, &Groups::none());
    assert_eq!(
        settle.accounts[12].pubkey,
        address::request(&v.vault, &address::claim(&v.vault, 7), 8)
    );
}

/// The app sends a legacy transaction, from one signer, with the compute
/// budget and (where the receiver has no dollar account) the account-opening
/// instruction in front. A claim's close at the stand-in venue with the
/// station's authority holding eight markets there doesn't fit the wire:
/// the builder says so, with the size, before anything is signed, which is
/// what the app measures before it offers "Settle now".
#[test]
fn a_claim_s_close_at_a_venue_holding_eight_markets_is_measured_too_large_before_signing() {
    use chain::solana_ix::budget::{open_dollars, with_budget};
    use chain::solana_ix::transaction::{TransactionError, Unsigned};
    use chain::solana_ix::vault::{Groups, Pinned, settle_claim, stand_in_group};

    let v = a_vault();
    let caller = Pubkey::new_unique();
    let claim = ClaimKeys {
        id: 3,
        owner: Pubkey::new_unique(),
        request: 0,
    };
    let venue = Pubkey::new_unique();
    let authority = address::venue_authority(&v.station, &venue);
    let pinned = Pinned {
        program: Pubkey::new_unique(),
        venue,
        authority,
        dollars: address::dollars(&authority, &v.mint),
    };
    let built = |markets: usize| {
        let held: Vec<Pubkey> = (0..markets).map(|_| Pubkey::new_unique()).collect();
        let mut groups = Groups::none();
        groups.push(stand_in_group(&pinned, &v.mint, &held));
        let ixs = with_budget(vec![
            open_dollars(&caller, &claim.owner, &v.mint),
            settle_claim(&v, &caller, &claim, &groups),
        ]);
        Unsigned::legacy(
            &caller,
            &ixs,
            chain::solana_ix::Hash::new_from_array([1; 32]),
        )
    };
    assert!(built(1).is_ok(), "one market fits");
    match built(8) {
        Err(TransactionError::TooLarge { size }) => assert!(size > 1232, "{size}"),
        other => panic!("eight markets measured as {:?}", other.map(|_| ())),
    }
}
