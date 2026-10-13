//! A send never uses a config other than the one its review was computed
//! with.
//!
//! A chain's config remembered from an earlier visit is what the page reads
//! at once; the API's fresh answer lands in the background, sooner or later.
//! A review (the exit panel's, a portfolio card's, an author's form) scales
//! its amounts with the decimals of the network it read, and its figures
//! came through that network's endpoint. So a flow carries the config of the
//! network its review was computed with, and stops before any wallet is
//! asked when the network it would sign with was read from another config,
//! whenever that config landed. The page then reads the new one, and the
//! person reviews again with it. When nothing changed, the send goes on with
//! exactly the network it was reviewed with.

use app::tx::error::TxError;
use app::tx::solana::SolError;
use app::wallet::config_cache::{ConfigSlot, ForSigning, canonical};
use futures::executor::block_on;
use shared::Chain;

async fn down() -> Result<String, String> {
    Err("the API answered 503".to_string())
}

/// A Base Sepolia config as the API writes it, with its dollar at
/// `0x{usd}…` and `decimals` decimals, in the form the page holds it.
fn base(decimals: u8, usd: u8) -> String {
    let address = |byte: u8| format!("{:?}", alloy_primitives::Address::repeat_byte(byte));
    canonical(
        &serde_json::json!({
            "chain": {"chain": "base", "key": "evm:84532", "chain_id": 84532, "block": 1, "as_of": [2026, 286, 8, 18, 18, 0]},
            "rpc_url": "https://sepolia.base.org",
            "addresses": {
                "usd": address(usd), "publishing": address(1), "prices": address(2),
                "venue": address(3), "attestation": address(4), "strategies": address(5),
                "reviews": address(6), "profiles": address(7), "factory": address(8),
                "station_factory": address(9),
            },
            "asset_symbol": "USDC",
            "asset_decimals": decimals,
            "terms": {
                "minimum_tank": 10.0, "publish_fee": 1.0,
                "fees": {"holding_bps": 100, "author_bps": 1000, "platform_bps": 500, "transaction_bps": 10},
                "exit_band_bps": 100, "freeze_after_days": 7.0, "image_timelock_days": 0.0,
                "minimum_deposit": 10.0, "terms_hash": format!("0x{}", "00".repeat(32)),
            },
            "dev": null,
        })
        .to_string(),
    )
    .expect("a JSON object")
}

/// A devnet config naming this build's programs, its dollar `mint` with
/// `decimals` decimals, in the form the page holds it.
fn devnet(decimals: u8, mint: u8) -> String {
    use chain::solana_ix::{Pubkey, ids};
    canonical(
        &serde_json::json!({
            "chain": {"chain": "solana", "key": "solana:devnet", "block": 510561479, "as_of": [2026, 286, 8, 14, 18, 0]},
            "rpc_url": "https://api.devnet.solana.com",
            "addresses": {
                "cluster": "devnet",
                "programs": {
                    "shonin_vault": ids::VAULT.to_string(),
                    "shonin_station": ids::STATION.to_string(),
                    "shonin_publishing": ids::PUBLISHING.to_string(),
                    "shonin_registry": ids::REGISTRY.to_string(),
                    "shonin_prices": ids::PRICES.to_string(),
                },
                "mint": Pubkey::new_from_array([mint; 32]).to_string(),
                "token_program": ids::TOKEN.to_string(),
                "vault_factory": Pubkey::new_from_array([1; 32]).to_string(),
                "publishing_config": Pubkey::new_from_array([2; 32]).to_string(),
            },
            "asset_symbol": "USDC",
            "asset_decimals": decimals,
            "terms": {
                "minimum_tank": 10.0, "publish_fee": 1.0,
                "fees": {"holding_bps": 100, "author_bps": 1000, "platform_bps": 500, "transaction_bps": 10},
                "exit_band_bps": 100, "freeze_after_days": 7.0, "image_timelock_days": 2.0,
                "minimum_deposit": 10.0, "terms_hash": format!("0x{}", "00".repeat(32)),
            },
            "dev": null,
        })
        .to_string(),
    )
    .expect("a JSON object")
}

/// The words a stopped send is said with on the exit panel.
fn said(error: &TxError) -> String {
    let words = app::components::exit::error_words(
        error,
        "MetaMask",
        "Base Sepolia",
        None,
        "USDC",
        6,
        "ETH",
    );
    format!("{} {}", words.title, words.text)
}

#[test]
fn a_fetch_that_replaced_the_remembered_copy_in_the_background_is_said_once_to_the_next_send() {
    let slot = ConfigSlot::new();
    // The page reads from the remembered copy: the exit panel opens on it.
    slot.remember(r#"{"asset_decimals":6,"rpc_url":"https://a"}"#);
    // The API's answer lands in the background and differs.
    let fresh = r#"{"asset_decimals":18,"rpc_url":"https://b"}"#;
    assert_eq!(
        block_on(slot.refresh(|| async move { Ok(fresh.to_string()) })),
        Ok(fresh.into())
    );
    // The next send hears that the config changed, however long after the
    // fetch landed it comes.
    assert_eq!(
        block_on(slot.for_signing(down)),
        ForSigning::Changed(fresh.into()),
        "a send after a fetch that disagreed with the remembered copy hears of it"
    );
    // Only once: the page has the fresh copy now.
    assert_eq!(
        block_on(slot.for_signing(down)),
        ForSigning::Use(fresh.into())
    );

    // A fetch that agreed with the remembered copy changes nothing.
    let slot = ConfigSlot::new();
    slot.remember(fresh);
    assert!(block_on(slot.refresh(|| async move { Ok(fresh.to_string()) })).is_ok());
    assert_eq!(
        block_on(slot.for_signing(down)),
        ForSigning::Use(fresh.into())
    );
}

#[test]
fn a_send_stops_before_the_wallet_when_a_fetch_in_the_background_replaced_the_config_its_review_read()
 {
    let session = app::wallet::get();
    session.remember(Chain::Base, &base(6, 0xe2));
    // The panel reviews with this copy: 6 decimals, dollar at 0xe2.
    let shown = block_on(session.network(Chain::Base)).expect("read from the remembered copy");
    assert_eq!(shown.asset_decimals, 6);
    // The API answers in the background with another dollar at 18 decimals.
    let landed = base(18, 0xe3);
    assert!(block_on(session.refresh_with(Chain::Base, || async move { Ok(landed) })).is_some());
    // The send, carrying the config its review read: it stops, saying the
    // settings were just updated, before any wallet is asked.
    match block_on(session.network_to_sign_with(Chain::Base, shown.config, down)) {
        Err(error @ TxError::SettingsChanged) => {
            let said = said(&error);
            assert!(said.contains("just updated"), "{said}");
            assert!(said.contains("Review it again"), "{said}");
            assert!(said.contains("Nothing was signed or sent."), "{said}");
        }
        Ok(network) => panic!(
            "signed with {} decimals and dollar {:?} while the review showed {} decimals and {:?}",
            network.asset_decimals, network.asset, shown.asset_decimals, shown.asset
        ),
        Err(other) => panic!("stopped for another reason: {other:?}"),
    }
}

#[test]
fn a_review_computed_on_one_config_stops_a_send_on_another_and_reviewing_again_sends_with_it() {
    let session = app::wallet::get();
    // Config A, fetched in this page's life: the review reads it.
    let a = base(6, 0xe2);
    assert!(block_on(session.refresh_with(Chain::Base, || async move { Ok(a) })).is_some());
    let reviewed = session.networks.lock_ref()[&Chain::Base].clone();
    // A deposit of 11 dollars, as the review scaled it.
    let amount = app::money::parse_units("11", reviewed.asset_decimals).unwrap();
    assert_eq!(amount, alloy_primitives::U256::from(11_000_000u64));
    // Config B lands before the send: another dollar, at 18 decimals.
    let b = base(18, 0xe3);
    assert!(block_on(session.refresh_with(Chain::Base, || async move { Ok(b) })).is_some());
    // The send carries A: it stops before the wallet, whatever the API
    // says now.
    assert_eq!(
        block_on(session.network_to_sign_with(Chain::Base, reviewed.config, down)),
        Err(TxError::SettingsChanged)
    );
    // "Try again": the page reads B, and the review is computed again with
    // it, from what was typed.
    let again = session.networks.lock_ref()[&Chain::Base].clone();
    assert_ne!(again.config, reviewed.config);
    assert_eq!(again.asset_decimals, 18);
    assert_eq!(again.asset, alloy_primitives::Address::repeat_byte(0xe3));
    let typed = app::money::format::plain_exact(amount, reviewed.asset_decimals);
    assert_eq!(typed, "11");
    let amount = app::money::parse_units(&typed, again.asset_decimals).unwrap();
    assert_eq!(
        amount,
        alloy_primitives::U256::from(11_000_000_000_000_000_000u128)
    );
    // That review's send goes on, with exactly the network it read.
    let signed = block_on(session.network_to_sign_with(Chain::Base, again.config, down)).unwrap();
    assert_eq!(*signed, *again);
}

/// Run `look` on a page of its own: a session with nothing read yet.
fn on_a_fresh_page(look: impl FnOnce() + Send + 'static) {
    std::thread::spawn(look).join().unwrap();
}

#[test]
fn a_send_whose_config_did_not_change_goes_on_with_exactly_the_network_it_was_reviewed_with() {
    // Fetched in this page's life, and reviewed with: the send uses it,
    // without asking the API again.
    on_a_fresh_page(|| {
        let session = app::wallet::get();
        let a = base(6, 0xe2);
        assert!(block_on(session.refresh_with(Chain::Base, || async move { Ok(a) })).is_some());
        let reviewed = session.networks.lock_ref()[&Chain::Base].clone();
        let signed =
            block_on(session.network_to_sign_with(Chain::Base, reviewed.config, down)).unwrap();
        assert_eq!(*signed, *reviewed);
        assert_eq!(signed.chain_id, 84532);
        assert_eq!(signed.rpc, "https://sepolia.base.org");
        assert_eq!(signed.asset, alloy_primitives::Address::repeat_byte(0xe2));
        assert_eq!(signed.asset_decimals, 6);
    });
    // Remembered, reviewed with, and the API answers the same config at
    // send time: the send goes on with it.
    on_a_fresh_page(|| {
        let session = app::wallet::get();
        session.remember(Chain::Base, &base(6, 0xe2));
        let reviewed = session.networks.lock_ref()[&Chain::Base].clone();
        let same = base(6, 0xe2);
        let signed = block_on(session.network_to_sign_with(
            Chain::Base,
            reviewed.config,
            || async move { Ok(same) },
        ))
        .unwrap();
        assert_eq!(*signed, *reviewed);
    });
    // Remembered, and the same config landed in the background before the
    // send: nothing changed, nothing stops.
    on_a_fresh_page(|| {
        let session = app::wallet::get();
        session.remember(Chain::Base, &base(6, 0xe2));
        let reviewed = session.networks.lock_ref()[&Chain::Base].clone();
        let same = base(6, 0xe2);
        assert!(block_on(session.refresh_with(Chain::Base, || async move { Ok(same) })).is_some());
        let signed =
            block_on(session.network_to_sign_with(Chain::Base, reviewed.config, down)).unwrap();
        assert_eq!(*signed, *reviewed);
    });
    // Remembered A, the fetch of B landed in the background, and the panel
    // followed it and reviewed with B: the send goes on with B, unstopped.
    on_a_fresh_page(|| {
        let session = app::wallet::get();
        session.remember(Chain::Base, &base(6, 0xe2));
        let b = base(18, 0xe3);
        assert!(block_on(session.refresh_with(Chain::Base, || async move { Ok(b) })).is_some());
        let reviewed = session.networks.lock_ref()[&Chain::Base].clone();
        assert_eq!(reviewed.asset_decimals, 18);
        let signed =
            block_on(session.network_to_sign_with(Chain::Base, reviewed.config, down)).unwrap();
        assert_eq!(*signed, *reviewed);
    });
}

#[test]
fn a_solana_send_stops_before_the_wallet_unless_the_cluster_is_the_one_its_review_read() {
    let session = app::wallet::solana::get();
    session.remember(&devnet(6, 7));
    let shown = session
        .network
        .get_cloned()
        .expect("read from the remembered copy");
    // The API's answer lands in the background: another dollar, 9 decimals.
    let landed = devnet(9, 8);
    assert!(block_on(session.refresh_with(|| async move { Ok(landed) })).is_some());
    assert_eq!(
        block_on(session.network_to_sign_with(shown.config, down)),
        Err(SolError::SettingsChanged)
    );
    // The page reads the new cluster; a review computed with it sends with
    // exactly it.
    let again = session.network.get_cloned().unwrap();
    assert_eq!(again.asset_decimals, 9);
    let signed = block_on(session.network_to_sign_with(again.config, down)).unwrap();
    assert_eq!(*signed, *again);
    // And once more with nothing changed: the same cluster.
    let signed = block_on(session.network_to_sign_with(again.config, down)).unwrap();
    assert_eq!(*signed, *again);
}
