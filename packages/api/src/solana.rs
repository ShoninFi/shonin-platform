//! The API's routes on a Solana vault chain: its config, read from the
//! programs' own accounts, and a holder's portfolio, read from their dollar
//! account and their holder account in each vault.

use axum::Json;
use axum::http::StatusCode;
use chain::solana::programs::anchor_lang::AccountDeserialize;
use chain::solana::programs::vault as sv;
use chain::solana::{Pubkey, Signer};
use chain::{Role, SolanaServed};
use shared::{
    ApiError, Chain, ChainInfo, Config, DevMode, HeldClaim, Holding, Portfolio, SolanaAddresses,
    Terms,
};

use crate::Service;

type Refused = (StatusCode, Json<ApiError>);

fn refused(status: StatusCode, message: String) -> Refused {
    (status, Json(ApiError { message }))
}

/// The clusters anyone can reach: a development wallet is never offered on
/// one of them.
const PUBLIC_CLUSTERS: [&str; 3] = ["mainnet-beta", "devnet", "testnet"];

pub async fn config(
    service: &Service,
    chain: Chain,
    served: &SolanaServed,
) -> Result<Config, Refused> {
    let terms = terms(served).await.map_err(|error| {
        tracing::warn!(%chain, error = format!("{error:#}"), "could not read the platform's terms");
        // An app told the wrong terms would state them as facts, so it is
        // told nothing until the cluster answers.
        refused(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("{} did not answer for the platform's terms", chain.label()),
        )
    })?;
    let info = service
        .indexer
        .snapshot
        .read()
        .await
        .chain(chain)
        .cloned()
        .unwrap_or(ChainInfo {
            chain,
            key: served.key.to_string(),
            chain_id: None,
            block: 0,
            as_of: None,
        });
    let deployments = &served.deployments;
    let client = &served.client;
    let viewer = (!PUBLIC_CLUSTERS.contains(&client.cluster.as_str()))
        .then(|| client.role(Role::Viewer).ok().map(|key| key.pubkey()))
        .flatten();
    Ok(Config {
        chain: info,
        // The file's own URL, never an override: a paid one carries its key.
        rpc_url: served.public_rpc.clone(),
        addresses: shared::Addresses::Solana(SolanaAddresses {
            cluster: client.cluster.clone(),
            programs: deployments.programs.clone(),
            mint: served.platform.mint.to_string(),
            token_program: deployments
                .token_program
                .clone()
                .unwrap_or_else(|| chain::solana::programs::anchor_spl::token::ID.to_string()),
            vault_factory: sv::state::factory_address().to_string(),
            publishing_config: chain::solana::platform::publishing_config().to_string(),
        }),
        asset_symbol: chain.dollar().to_string(),
        asset_decimals: sv::state::DECIMALS,
        terms,
        dev: viewer.map(|key| DevMode {
            address: key.to_string(),
            label: "Development wallet".to_string(),
        }),
    })
}

/// The terms the cluster's programs enforce, read from their accounts.
async fn terms(served: &SolanaServed) -> anyhow::Result<Terms> {
    const DAY: f64 = 86_400.0;
    let client = &served.client;
    let platform = &served.platform;
    let factory = platform.factory(client).await?;
    let publishing = platform.publishing_config(client).await?;
    let registry = platform.registry_config(client).await?;
    let now = client.now().await?;
    let terms = &factory.terms;
    Ok(Terms {
        minimum_tank: dollars(terms.minimum_tank),
        publish_fee: dollars(publishing.fee),
        fees: shared::Fees {
            holding_bps: terms.fees.holding_bps as u32,
            author_bps: terms.fees.author_bps as u32,
            platform_bps: terms.fees.platform_bps as u32,
            transaction_bps: terms.fees.transaction_bps as u32,
        },
        exit_band_bps: terms.exit_band_bps as u32,
        freeze_after_days: terms.timers.freeze_after as f64 / DAY,
        image_timelock_days: registry.timelock(now) as f64 / DAY,
        minimum_deposit: dollars(terms.minimum_deposit),
        terms_hash: format!("0x{}", alloy::hex::encode(sv::factory::terms_hash(terms))),
    })
}

fn dollars(units: u64) -> f64 {
    units as f64 / 1e6
}

/// What a Solana account holds on this cluster: the dollar in its own token
/// account, each vault's shares in its holder account there, what it paid
/// for them (from the vault's history as the indexer has read it), and the
/// claims it waits on.
pub async fn portfolio(
    service: &Service,
    chain: Chain,
    served: &SolanaServed,
    address: String,
) -> Result<Json<Portfolio>, Refused> {
    let who: Pubkey = address.parse().map_err(|_| {
        refused(
            StatusCode::BAD_REQUEST,
            "that is not an address".to_string(),
        )
    })?;
    let client = &served.client;
    // A balance that could not be read is not a balance of nothing: the
    // portfolio is not answered rather than shown empty.
    let unread = |error: anyhow::Error| {
        tracing::warn!(%chain, error = format!("{error:#}"), "a portfolio balance could not be read");
        refused(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("{} did not answer", chain.label()),
        )
    };
    let balance = dollars(
        client
            .token_balance(&served.platform.dollars(&who))
            .await
            .map_err(unread)?,
    );

    // The chain's strategies are copied out and the record let go before the
    // cluster is read, as on the EVM.
    let strategies: Vec<_> = service
        .indexer
        .snapshot
        .read()
        .await
        .strategies
        .iter()
        .filter(|strategy| strategy.vault.chain == chain)
        .filter(|strategy| !indexer::read::never_read(strategy))
        .cloned()
        .collect();
    let vaults: Vec<Option<Pubkey>> = strategies
        .iter()
        .map(|strategy| strategy.vault.address.parse().ok())
        .collect();
    let holders: Vec<Pubkey> = vaults
        .iter()
        .flatten()
        .map(|vault| sv::state::holder_address(vault, &who).0)
        .collect();
    let mut accounts = Vec::new();
    for chunk in holders.chunks(100) {
        accounts.extend(client.rpc.accounts(chunk).await.map_err(unread)?);
    }
    let mut accounts = accounts.into_iter();

    let mut positions = Vec::new();
    let mut claims = Vec::new();
    for (strategy, vault) in strategies.iter().zip(vaults) {
        let Some(vault) = vault else {
            continue;
        };
        let shares = match accounts.next().flatten() {
            Some(account) if !account.data.is_empty() => {
                sv::state::Holder::try_deserialize(&mut &account.data[..])
                    .map_err(|error| unread(anyhow::anyhow!("a holder account: {error}")))?
                    .shares
            }
            _ => 0,
        };
        let held = shares as f64 / 1e18;
        if held > 0.0 {
            let cost = service
                .indexer
                .solana_cost(chain, &vault, &who)
                .await
                .unwrap_or(0.0);
            positions.push(Holding {
                slug: strategy.slug.clone(),
                shares: held,
                value: held * strategy.vault.share_price,
                exit_value: held
                    * strategy.vault.share_price
                    * (1.0 - strategy.vault.fees.transaction_bps as f64 / 10_000.0),
                cost,
            });
        }
        for claim in &strategy.vault.claims {
            // A key is its base58 spelling: compared exactly.
            if claim.owner == address && claim.state != shared::model::vault::ClaimState::Paid {
                claims.push(HeldClaim {
                    slug: strategy.slug.clone(),
                    id: claim.id as u64,
                    shares: claim.shares,
                    fraction_closed: claim.fraction_closed,
                    state: claim.state,
                    booked: claim.booked.midnight(),
                    settled_for: claim.settled_for,
                });
            }
        }
    }
    Ok(Json(Portfolio {
        chain,
        address,
        balance,
        positions,
        claims,
    }))
}
