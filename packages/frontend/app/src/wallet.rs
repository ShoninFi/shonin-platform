//! The viewer's wallet, and what it can do.
//!
//! Every action here is a transaction against the vault's own contract:
//! depositing, leaving, settling a claim someone booked, moving to a new
//! version, topping up a tank, buying a review. Nothing is simulated and
//! nothing is kept in the browser — what the viewer holds is read back from
//! the chain, because that is where it is.
//!
//! Two ways to connect, and they send the same transactions to the same
//! contracts. A **browser wallet** is whatever the viewer has installed. A
//! **development wallet** is an account the local chain already holds the key
//! for, which is what makes it a development chain and why the app can offer
//! it: it appears only when the API says so, and never anywhere else.

#[cfg(target_arch = "wasm32")]
use std::rc::Rc;

use futures_signals::signal::Mutable;
use shared::model::vault::{ClaimState, Vault};
use shared::{HeldClaim, Holding, Portfolio};

#[cfg(target_arch = "wasm32")]
use crate::data;

/// How the viewer is connected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Connection {
    /// A wallet the viewer has installed.
    Browser,
    /// An account the development chain hands out.
    Development,
    /// Reading someone else's holdings, signing nothing.
    Watching,
}

impl Connection {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Browser => "Browser wallet",
            Self::Development => "Development wallet",
            Self::Watching => "Watching",
        }
    }

    /// Whether this connection can send a transaction at all.
    pub fn can_sign(&self) -> bool {
        !matches!(self, Self::Watching)
    }
}

/// What the viewer holds, read from the chain.
#[derive(Clone, Debug, Default)]
pub struct Holdings {
    pub address: Option<String>,
    pub how: Option<Connection>,
    /// The asset, outside any vault.
    pub balance: f64,
    pub positions: Vec<Holding>,
    pub claims: Vec<HeldClaim>,
}

impl Holdings {
    pub fn is_connected(&self) -> bool {
        self.address.is_some()
    }

    pub fn can_sign(&self) -> bool {
        self.how.as_ref().is_some_and(Connection::can_sign)
    }

    pub fn position(&self, slug: &str) -> Option<&Holding> {
        self.positions.iter().find(|held| held.slug == slug)
    }

    pub fn claims_for(&self, slug: &str) -> Vec<&HeldClaim> {
        self.claims
            .iter()
            .filter(|claim| claim.slug == slug)
            .collect()
    }

    /// What every position is worth at today's value per share.
    pub fn value(&self) -> f64 {
        self.positions.iter().map(|held| held.value).sum()
    }

    /// What they would fetch on the way out.
    pub fn exit_value(&self) -> f64 {
        self.positions.iter().map(|held| held.exit_value).sum()
    }

    /// What those positions cost, read from the vaults' own logs.
    pub fn cost(&self) -> f64 {
        self.positions.iter().map(|held| held.cost).sum()
    }
}

thread_local! {
    static HOLDINGS: Mutable<Holdings> = Mutable::new(Holdings::default());
}

/// What the viewer holds. Read it with `holdings().signal_cloned()`.
pub fn holdings() -> Mutable<Holdings> {
    HOLDINGS.with(|holdings| holdings.clone())
}

#[cfg(target_arch = "wasm32")]
/// Which connection was last used, so following a link does not disconnect
/// the viewer. Only the choice and the address are remembered, and both are
/// public; no key is ever anywhere near the browser's storage.
fn remember(address: &str, how: &Connection) {
    let kind = match how {
        Connection::Browser => "browser",
        Connection::Development => "development",
        Connection::Watching => "watching",
    };
    crate::state::storage_set("wallet", &format!("{kind}:{address}"));
}

pub fn forget() {
    crate::state::storage_remove("wallet");
}

/// What was remembered, if anything.
pub fn remembered() -> Option<(Connection, String)> {
    let held = crate::state::storage_get("wallet")?;
    let (kind, address) = held.split_once(':')?;
    let how = match kind {
        "browser" => Connection::Browser,
        "development" => Connection::Development,
        "watching" => Connection::Watching,
        _ => return None,
    };
    Some((how, address.to_string()))
}

#[cfg(target_arch = "wasm32")]
fn set_address(address: String, how: Connection) {
    remember(&address, &how);
    holdings().replace_with(|holdings| {
        holdings.address = Some(address);
        holdings.how = Some(how);
        holdings.clone()
    });
}

pub fn disconnect() {
    forget();
    holdings().set(Holdings::default());
}

/// Take what the API read from each vault chain as the viewer's holdings: the
/// balance on every chain together, and every position and claim.
pub fn apply(portfolios: Vec<Portfolio>) {
    holdings().replace_with(|holdings| {
        holdings.balance = portfolios.iter().map(|portfolio| portfolio.balance).sum();
        holdings.positions = portfolios
            .iter()
            .flat_map(|portfolio| portfolio.positions.clone())
            .collect();
        holdings.claims = portfolios
            .iter()
            .flat_map(|portfolio| portfolio.claims.clone())
            .collect();
        holdings.clone()
    });
}

/// A fraction of the spread a leaver pays by being valued at exit-side marks.
/// The contract works out the real one from the venue; this is what a quote
/// shows before the transaction is sent.
const EXIT_SPREAD: f64 = 0.0008;

/// What a deposit of this much would do.
pub struct DepositQuote {
    pub assets: f64,
    pub fee: f64,
    pub shares: f64,
    pub share_price: f64,
}

pub fn quote_deposit(vault: &Vault, assets: f64) -> DepositQuote {
    let fee = assets * fee_fraction(vault);
    DepositQuote {
        assets,
        fee,
        // Deposits are valued at mark, in the same transaction.
        shares: if vault.share_price > 0.0 {
            (assets - fee) / vault.share_price
        } else {
            0.0
        },
        share_price: vault.share_price,
    }
}

/// What a withdrawal of this many shares would do.
pub struct WithdrawQuote {
    pub shares: f64,
    /// The price a leaver is valued at: the exit side of the spread.
    pub exit_price: f64,
    pub gross: f64,
    pub fee: f64,
    pub assets: f64,
    /// True when the idle balance cannot cover it, so a claim is booked.
    pub books_claim: bool,
}

pub fn quote_withdraw(vault: &Vault, shares: f64) -> WithdrawQuote {
    let exit_price = vault.share_price * (1.0 - EXIT_SPREAD);
    let gross = shares * exit_price;
    let fee = gross * fee_fraction(vault);
    WithdrawQuote {
        shares,
        exit_price,
        gross,
        fee,
        assets: gross - fee,
        books_claim: gross > vault.idle,
    }
}

fn fee_fraction(vault: &Vault) -> f64 {
    f64::from(vault.fees.transaction_bps) / 10_000.0
}

/// What the transaction fee comes to, as a percentage, for showing in a quote.
pub fn transaction_fee_label(vault: &Vault) -> String {
    crate::format::from_bps(vault.fees.transaction_bps)
}

/// Whether a claim is still waiting on something.
pub fn is_outstanding(claim: &HeldClaim) -> bool {
    !matches!(claim.state, ClaimState::Paid | ClaimState::Fallback)
}

// Sending things -------------------------------------------------------------

/// What the app is waiting on, so a page can say so rather than look frozen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pending {
    Nothing,
    Sending(String),
}

thread_local! {
    static PENDING: Mutable<Pending> = Mutable::new(Pending::Nothing);
}

pub fn pending() -> Mutable<Pending> {
    PENDING.with(|pending| pending.clone())
}

pub use send::{
    activate, claim_closed, claim_instead, connect, deposit, fund_tank, refresh, resume, retire,
    set_allowances, set_idle_floor, set_tank_keep, settle, submit, take_pro_rata,
    take_pro_rata_claim, withdraw, withdraw_tank,
};

/// The same surface where there is no browser to ask. The build-time
/// pre-render compiles the whole app natively to write its pages out; nothing
/// there sends anything, and saying so plainly beats pretending.
#[cfg(not(target_arch = "wasm32"))]
mod send {
    use super::*;

    const NOWHERE: &str = "this build has no wallet to ask";

    pub async fn connect(_how: Connection, _watching: Option<String>) -> Result<(), String> {
        Err(NOWHERE.to_string())
    }
    pub async fn refresh() {}
    pub async fn resume() {}
    pub async fn deposit(_slug: &str, _assets: f64) -> Result<String, String> {
        Err(NOWHERE.to_string())
    }
    pub async fn withdraw(_slug: &str, _shares: f64) -> Result<String, String> {
        Err(NOWHERE.to_string())
    }
    pub async fn settle(_slug: &str, _claim: u64) -> Result<String, String> {
        Err(NOWHERE.to_string())
    }
    pub async fn claim_closed(_slug: &str, _claim: u64) -> Result<String, String> {
        Err(NOWHERE.to_string())
    }
    pub async fn take_pro_rata(_slug: &str, _request: u64) -> Result<String, String> {
        Err(NOWHERE.to_string())
    }
    pub async fn claim_instead(_slug: &str, _request: u64) -> Result<String, String> {
        Err(NOWHERE.to_string())
    }
    pub async fn take_pro_rata_claim(_slug: &str, _claim: u64) -> Result<String, String> {
        Err(NOWHERE.to_string())
    }
    pub async fn fund_tank(_slug: &str, _assets: f64) -> Result<String, String> {
        Err(NOWHERE.to_string())
    }
    pub async fn set_tank_keep(_slug: &str, _assets: f64) -> Result<String, String> {
        Err(NOWHERE.to_string())
    }
    pub async fn withdraw_tank(_slug: &str, _assets: f64) -> Result<String, String> {
        Err(NOWHERE.to_string())
    }
    pub async fn set_allowances(_slug: &str, _shape: &str) -> Result<String, String> {
        Err(NOWHERE.to_string())
    }
    pub async fn set_idle_floor(_slug: &str, _bps: u16) -> Result<String, String> {
        Err(NOWHERE.to_string())
    }
    pub async fn retire(_slug: &str) -> Result<String, String> {
        Err(NOWHERE.to_string())
    }
    pub async fn submit(
        _chain: shared::Chain,
        _archive: &[u8],
        _sealed: bool,
    ) -> Result<String, String> {
        Err(NOWHERE.to_string())
    }
    pub async fn activate(
        _chain: shared::Chain,
        _id: u64,
        _allowance: &str,
        _tank: f64,
        _floor_bps: u16,
    ) -> Result<String, String> {
        Err(NOWHERE.to_string())
    }
}

/// Everything that puts a transaction on the chain.
#[cfg(target_arch = "wasm32")]
mod send {
    use alloy_primitives::{Address, U256};
    use alloy_sol_types::SolCall;
    use chain::bindings::erc20::IERC20;
    use chain::bindings::factory::IVaultFactory;
    use chain::bindings::publishing::IPublishing;
    use chain::bindings::vault::IVault;
    use wasm_bindgen::JsValue;
    use wasm_bindgen_futures::JsFuture;

    use super::*;
    use crate::api;

    /// Connect, and read back what this address holds.
    pub async fn connect(how: Connection, watching: Option<String>) -> Result<(), String> {
        let address = match &how {
            Connection::Browser => ask_the_browser().await?,
            Connection::Development => data::dev()
                .map(|dev| dev.address)
                .ok_or("this platform offers no development wallet")?,
            Connection::Watching => watching
                .filter(|address| address.len() == 42 && address.starts_with("0x"))
                .ok_or("that is not an address")?,
        };
        set_address(address, how);
        refresh().await;
        Ok(())
    }

    /// Pick up where the viewer left off. A browser wallet is asked again,
    /// because only it can say whether it still agrees; the others are simply
    /// resumed, since neither signs anything a wallet did not already allow.
    pub async fn resume() {
        let Some((how, address)) = remembered() else {
            return;
        };
        match how {
            Connection::Browser => {
                let _ = connect(Connection::Browser, None).await;
            }
            other => {
                set_address(address, other);
                refresh().await;
            }
        }
    }

    /// Read the viewer's holdings back from the chain.
    pub async fn refresh() {
        let Some(address) = holdings().lock_ref().address.clone() else {
            return;
        };
        // One portfolio per vault chain, read together.
        let mut portfolios = Vec::new();
        for config in data::configs() {
            if let Ok(portfolio) = api::portfolio(config.chain.chain, &address).await {
                portfolios.push(portfolio);
            }
        }
        apply(portfolios);
    }

    /// Ask to put money into a vault: the depositor signs the statement for
    /// the vault's list, the asset is approved, and the request waits for the
    /// next valuation to price it.
    pub async fn deposit(slug: &str, assets: f64) -> Result<String, String> {
        let (strategy, from) = about(slug)?;
        let chain = strategy.vault.chain;
        let vault = address_of(&strategy.vault.address)?;
        let amount = units(assets);
        let usd = asset_address(chain)?;

        let restricted = read(chain, vault, IVault::restrictedTextCall {}).await?;
        working("Signing the statement");
        let statement = sign_statement(chain, &from, vault, &restricted).await?;

        working("Approving");
        send(
            chain,
            &from,
            usd,
            IERC20::approveCall {
                spender: vault,
                value: amount,
            }
            .abi_encode(),
        )
        .await?;

        working("Asking to deposit");
        let hash = send(
            chain,
            &from,
            vault,
            IVault::requestDepositCall {
                assets: amount,
                receiver: address_of(&from)?,
                statement,
            }
            .abi_encode(),
        )
        .await;
        done();
        refresh().await;
        hash
    }

    /// Ask to leave a vault. The next valuation prices it: paid from cash if
    /// the cash covers it, and booked as a claim if it does not. A closed
    /// vault is all cash and pays at once.
    pub async fn withdraw(slug: &str, shares: f64) -> Result<String, String> {
        let (strategy, from) = about(slug)?;
        let chain = strategy.vault.chain;
        let vault = address_of(&strategy.vault.address)?;

        let state = read(chain, vault, IVault::stateCall {}).await?;
        if matches!(state, chain::bindings::vault::VaultState::Closed) {
            working("Withdrawing from the closed vault");
            let hash = send(
                chain,
                &from,
                vault,
                IVault::redeemClosedCall {
                    shares: share_units(shares),
                    receiver: address_of(&from)?,
                }
                .abi_encode(),
            )
            .await;
            done();
            refresh().await;
            return hash;
        }

        working("Asking to withdraw");
        let hash = send(
            chain,
            &from,
            vault,
            IVault::requestRedeemCall {
                shares: share_units(shares),
                receiver: address_of(&from)?,
            }
            .abi_encode(),
        )
        .await;
        done();
        refresh().await;
        hash
    }

    /// Serve a claim: close the leaver's fraction and bring it home. Anyone
    /// may send this, which is the point of it.
    pub async fn settle(slug: &str, claim: u64) -> Result<String, String> {
        let (strategy, from) = about(slug)?;
        let chain = strategy.vault.chain;
        let vault = address_of(&strategy.vault.address)?;

        working("Serving the claim");
        let hash = send(
            chain,
            &from,
            vault,
            IVault::settleClaimCall {
                claimId: U256::from(claim),
            }
            .abi_encode(),
        )
        .await;
        done();
        refresh().await;
        hash
    }

    /// A claim still unpaid when its vault closed, paid from the closed
    /// vault's cash. Anyone may send it; it pays the claim's owner.
    pub async fn claim_closed(slug: &str, claim: u64) -> Result<String, String> {
        holder_call(slug, "Collecting from the closed vault", |_| {
            IVault::claimClosedCall {
                claimId: U256::from(claim),
            }
            .abi_encode()
        })
        .await
    }

    /// A withdrawal nothing has priced for the fallback delay: the holder's
    /// share of the cash now, and units for their share of what is at the
    /// stations.
    pub async fn take_pro_rata(slug: &str, request: u64) -> Result<String, String> {
        holder_call(slug, "Taking your share now", |_| {
            IVault::takeProRataCall {
                requestId: U256::from(request),
            }
            .abi_encode()
        })
        .await
    }

    /// The same withdrawal, made a claim instead: a close is tried for it,
    /// and it counts toward freezing a vault that leaves it unpaid.
    pub async fn claim_instead(slug: &str, request: u64) -> Result<String, String> {
        holder_call(slug, "Making it a claim", |_| {
            IVault::claimInsteadCall {
                requestId: U256::from(request),
            }
            .abi_encode()
        })
        .await
    }

    /// A claim nobody could serve for the fallback delay from its booking:
    /// its share of the cash now, and units for the rest.
    pub async fn take_pro_rata_claim(slug: &str, claim: u64) -> Result<String, String> {
        holder_call(slug, "Taking your share now", |_| {
            IVault::takeProRataClaimCall {
                claimId: U256::from(claim),
            }
            .abi_encode()
        })
        .await
    }

    async fn holder_call(
        slug: &str,
        what: &str,
        call: impl Fn(Address) -> Vec<u8>,
    ) -> Result<String, String> {
        // The contract says when it isn't the holder's to send, or not yet.
        let (strategy, from) = about(slug)?;
        let vault = address_of(&strategy.vault.address)?;
        working(what);
        let hash = send(strategy.vault.chain, &from, vault, call(vault)).await;
        done();
        refresh().await;
        hash
    }

    // The author's controls --------------------------------------------------

    /// "Keep my tank at": topped back up from the author's shares. Zero is
    /// off.
    pub async fn set_tank_keep(slug: &str, assets: f64) -> Result<String, String> {
        author_call(slug, "Setting the tank's level", |_| {
            IVault::setTankKeepCall {
                assets: units(assets),
            }
            .abi_encode()
        })
        .await
    }

    /// Take money out of the tank, after the bill so far is settled with the
    /// executor's latest signed total.
    pub async fn withdraw_tank(slug: &str, assets: f64) -> Result<String, String> {
        let (strategy, _) = about(slug)?;
        let total = api::usage(strategy.vault.chain, &strategy.vault.address)
            .await
            .map_err(|why| format!("the executor's bill could not be read: {why}"))?;
        let signature: alloy_primitives::Bytes = total
            .signature
            .parse()
            .map_err(|_| "the executor's bill was not signed".to_string())?;
        let amount = units(assets);
        let usage = total.total.parse::<U256>().unwrap_or_default();
        author_call(slug, "Withdrawing from the tank", move |_| {
            IVault::withdrawTankCall {
                assets: amount,
                total: usage,
                asOf: total.as_of,
                signature: signature.clone(),
            }
            .abi_encode()
        })
        .await
    }

    /// What the agent may use, which is the most its tank can be charged.
    pub async fn set_allowances(slug: &str, shape: &str) -> Result<String, String> {
        let allowances = chain::allowances(&shared::Allowances::shape(shape));
        author_call(slug, "Setting the allowances", move |_| {
            IVault::setAllowancesCall {
                allowances: allowances.clone(),
            }
            .abi_encode()
        })
        .await
    }

    /// The share of value kept as cash. Raising it applies at once; lowering
    /// it waits a day.
    pub async fn set_idle_floor(slug: &str, bps: u16) -> Result<String, String> {
        author_call(slug, "Setting the idle floor", move |_| {
            IVault::setIdleFloorCall { bps }.abi_encode()
        })
        .await
    }

    /// Stop taking deposits and wind the vault down.
    pub async fn retire(slug: &str) -> Result<String, String> {
        author_call(slug, "Retiring", |_| IVault::retireCall {}.abi_encode()).await
    }

    async fn author_call(
        slug: &str,
        what: &str,
        call: impl Fn(Address) -> Vec<u8>,
    ) -> Result<String, String> {
        // The contract refuses anyone but the author, and says so.
        let (strategy, from) = about(slug)?;
        let vault = address_of(&strategy.vault.address)?;
        working(what);
        let hash = send(strategy.vault.chain, &from, vault, call(vault)).await;
        done();
        refresh().await;
        hash
    }

    /// Top up the tank that pays a strategy's running costs. Anyone may.
    pub async fn fund_tank(slug: &str, assets: f64) -> Result<String, String> {
        let (strategy, from) = about(slug)?;
        let chain = strategy.vault.chain;
        let vault = address_of(&strategy.vault.address)?;
        let amount = units(assets);
        let usd = asset_address(chain)?;

        working("Approving");
        send(
            chain,
            &from,
            usd,
            IERC20::approveCall {
                spender: vault,
                value: amount,
            }
            .abi_encode(),
        )
        .await?;

        working("Funding the tank");
        let hash = send(
            chain,
            &from,
            vault,
            IVault::fundTankCall { assets: amount }.abi_encode(),
        )
        .await;
        done();
        refresh().await;
        hash
    }

    /// Submit an agent: upload its source, then pay the $1 against the upload
    /// from the same wallet, which admits the job and gives it its id. Returns
    /// the job.
    pub async fn submit(
        chain: shared::Chain,
        archive: &[u8],
        sealed: bool,
    ) -> Result<String, String> {
        let from = signer()?;
        let setup = setup(chain)?;
        let publishing = address_of(&evm(&setup)?.publishing)?;
        let usd = asset_address(chain)?;

        working("Uploading the source");
        let accepted = api::upload(
            chain,
            &shared::Upload {
                author: from.clone(),
                archive_base64: shared::base64::encode(archive),
                sealed,
                package: None,
            },
        )
        .await
        .inspect_err(|_| done())?;
        let upload: alloy_primitives::B256 = accepted
            .upload
            .parse()
            .map_err(|_| "the build service's reference".to_string())?;

        working("Approving the $1");
        send(
            chain,
            &from,
            usd,
            IERC20::approveCall {
                spender: publishing,
                value: units(setup.terms.publish_fee),
            }
            .abi_encode(),
        )
        .await?;

        working("Paying to publish");
        send(
            chain,
            &from,
            publishing,
            IPublishing::submitCall { upload }.abi_encode(),
        )
        .await?;
        done();
        Ok(accepted.job_id)
    }

    /// Activate a reviewed and built agent: create its vault, with the
    /// allowances its tank is charged against, its idle floor and a starting
    /// tank, on the terms the author has read. The author signs the
    /// depositor's statement first, like every holder. Returns the agent's
    /// slug.
    pub async fn activate(
        chain: shared::Chain,
        id: u64,
        allowance: &str,
        tank: f64,
        floor_bps: u16,
    ) -> Result<String, String> {
        let from = signer()?;
        let config = setup(chain)?;
        let factory = address_of(&evm(&config)?.factory)?;
        let usd = asset_address(chain)?;
        let funding = units(tank);
        let strategy_id = U256::from(id);

        let vault = read(
            chain,
            factory,
            IVaultFactory::predictVaultCall {
                strategyId: strategy_id,
            },
        )
        .await?;
        let restricted = read(
            chain,
            factory,
            IVaultFactory::restrictedForCall {
                strategyId: strategy_id,
            },
        )
        .await?
        .text;
        let terms: alloy_primitives::B256 = config
            .terms
            .terms_hash
            .parse()
            .map_err(|_| "the platform's terms could not be read".to_string())?;

        working("Signing the statement");
        let statement = sign_statement(chain, &from, vault, &restricted).await?;

        working("Approving the tank");
        send(
            chain,
            &from,
            usd,
            IERC20::approveCall {
                spender: factory,
                value: funding,
            }
            .abi_encode(),
        )
        .await?;

        working("Activating");
        send(
            chain,
            &from,
            factory,
            IVaultFactory::activateCall {
                strategyId: strategy_id,
                allowances: chain::allowances(&shared::Allowances::shape(allowance)),
                idleFloorBps: floor_bps,
                tankFunding: funding,
                tankKeep: U256::ZERO,
                statement,
                terms,
            }
            .abi_encode(),
        )
        .await?;
        done();
        Ok(shared::Strategy::slug_for(chain, id))
    }

    /// The depositor's statement, as the vault checks it: typed data a wallet
    /// shows the person, with the words and the list they are agreeing to.
    async fn sign_statement(
        chain: shared::Chain,
        from: &str,
        vault: Address,
        restricted: &str,
    ) -> Result<alloy_primitives::Bytes, String> {
        let config = setup(chain)?;
        let typed = serde_json::json!({
            "types": {
                "EIP712Domain": [
                    {"name": "name", "type": "string"},
                    {"name": "version", "type": "string"},
                    {"name": "chainId", "type": "uint256"},
                    {"name": "verifyingContract", "type": "address"},
                ],
                "Statement": [
                    {"name": "depositor", "type": "address"},
                    {"name": "statement", "type": "string"},
                    {"name": "restricted", "type": "string"},
                ],
            },
            "primaryType": "Statement",
            "domain": {
                "name": "Shonin Vault",
                "version": "1",
                "chainId": evm_chain_id(&config)?,
                "verifyingContract": format!("{vault:?}"),
            },
            "message": {
                "depositor": from,
                "statement": chain::statement::TEXT,
                "restricted": restricted,
            },
        });
        let params = serde_json::json!([from, typed.to_string()]);
        let how = holdings().lock_ref().how.clone();
        let signature = match how {
            Some(Connection::Browser) => ask_wallet("eth_signTypedData_v4", params).await?,
            Some(Connection::Development) => {
                ask_chain(chain, "eth_signTypedData_v4", params).await?
            }
            _ => return Err("This connection signs nothing.".to_string()),
        };
        signature
            .parse()
            .map_err(|_| "the wallet's signature could not be read".to_string())
    }

    /// Ask a contract something, through the chain's own endpoint.
    async fn read<C: SolCall>(
        chain: shared::Chain,
        to: Address,
        call: C,
    ) -> Result<C::Return, String> {
        let request = serde_json::json!([{
            "to": format!("{to:?}"),
            "data": format!("0x{}", alloy_primitives::hex::encode(call.abi_encode())),
        }, "latest"]);
        let answer = ask_chain(chain, "eth_call", request).await?;
        let bytes = alloy_primitives::hex::decode(answer.trim_start_matches("0x"))
            .map_err(|_| "the chain answered in something other than hex".to_string())?;
        C::abi_decode_returns(&bytes).map_err(|error| error.to_string())
    }

    /// The connected address, if it can sign.
    fn signer() -> Result<String, String> {
        let holdings = holdings();
        let held = holdings.lock_ref();
        if !held.can_sign() {
            return Err("Connect a wallet that can sign first.".to_string());
        }
        held.address
            .clone()
            .ok_or_else(|| "Connect a wallet first.".to_string())
    }

    // The plumbing -----------------------------------------------------------

    fn working(what: &str) {
        pending().set(Pending::Sending(what.to_string()));
    }

    fn done() {
        pending().set(Pending::Nothing);
    }

    /// The strategy a slug names, and the address acting on it.
    fn about(slug: &str) -> Result<(Rc<shared::Strategy>, String), String> {
        let strategy = data::strategy(slug).ok_or("That vault is not on this chain.")?;
        let holdings = holdings();
        let holdings = holdings.lock_ref();
        if !holdings.can_sign() {
            return Err("Connect a wallet that can sign first.".to_string());
        }
        let from = holdings
            .address
            .clone()
            .ok_or("Connect a wallet first.".to_string())?;
        Ok((strategy, from))
    }

    /// How to reach a vault chain.
    fn setup(chain: shared::Chain) -> Result<Rc<shared::Config>, String> {
        data::config(chain).ok_or_else(|| {
            format!(
                "the app has not reached {} on this platform yet",
                chain.label()
            )
        })
    }

    fn asset_address(chain: shared::Chain) -> Result<Address, String> {
        let config = setup(chain)?;
        address_of(&evm(&config)?.usd)
    }

    /// The contracts of an EVM vault chain: the wallet signs only for EVM
    /// chains, so a chain of another kind is refused here.
    fn evm(config: &shared::Config) -> Result<&shared::EvmAddresses, String> {
        config.addresses.evm().ok_or_else(|| {
            format!(
                "the app's wallet does not sign for {} yet",
                config.chain.chain.label()
            )
        })
    }

    /// The EVM chain id typed data names.
    fn evm_chain_id(config: &shared::Config) -> Result<u64, String> {
        config.chain.chain_id.ok_or_else(|| {
            format!(
                "the app's wallet does not sign for {} yet",
                config.chain.chain.label()
            )
        })
    }

    fn address_of(text: &str) -> Result<Address, String> {
        text.parse()
            .map_err(|_| format!("{text} is not an address"))
    }

    /// The asset's own units: six decimals.
    fn units(amount: f64) -> U256 {
        U256::from((amount.max(0.0) * 1e6).round() as u128)
    }

    /// Shares carry eighteen.
    fn share_units(shares: f64) -> U256 {
        U256::from((shares.max(0.0) * 1e18).round() as u128)
    }

    /// Send a transaction, whichever way the viewer is connected.
    ///
    /// Both paths ask for the same thing: the wallet holds the key and signs.
    /// A browser wallet is asked through the page; the development chain is
    /// asked directly, because it holds the key itself.
    async fn send(
        chain: shared::Chain,
        from: &str,
        to: Address,
        data: Vec<u8>,
    ) -> Result<String, String> {
        let request = serde_json::json!({
            "from": from,
            "to": format!("{to:?}"),
            "data": format!("0x{}", alloy_primitives::hex::encode(data)),
        });

        let how = holdings().lock_ref().how.clone();
        let params = serde_json::json!([request]);
        match how {
            Some(Connection::Browser) => ask_wallet("eth_sendTransaction", params).await,
            Some(Connection::Development) => ask_chain(chain, "eth_sendTransaction", params).await,
            _ => Err("This connection signs nothing.".to_string()),
        }
    }

    /// Ask the wallet the viewer has installed.
    async fn ask_wallet(method: &str, params: serde_json::Value) -> Result<String, String> {
        let provider = injected().ok_or("no browser wallet is installed")?;
        let arguments = js_sys::Object::new();
        set(&arguments, "method", &JsValue::from_str(method));
        set(&arguments, "params", &to_js(&params)?);

        let call = js_sys::Reflect::get(&provider, &JsValue::from_str("request"))
            .map_err(|_| "that wallet does not take requests".to_string())?;
        let call: js_sys::Function = call
            .dyn_into()
            .map_err(|_| "that wallet does not take requests".to_string())?;
        let answer = call
            .call1(&provider, &arguments)
            .map_err(|error| said(&error))?;
        let answer = JsFuture::from(js_sys::Promise::from(answer))
            .await
            .map_err(|error| said(&error))?;
        Ok(answer.as_string().unwrap_or_default())
    }

    /// Ask the chain directly: on a development chain it holds the key
    /// itself, and anyone may read from it.
    async fn ask_chain(
        chain: shared::Chain,
        method: &str,
        params: serde_json::Value,
    ) -> Result<String, String> {
        let setup = setup(chain)?;
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        let reply: serde_json::Value = gloo_net::http::Request::post(&setup.rpc_url)
            .json(&body)
            .map_err(|error| error.to_string())?
            .send()
            .await
            .map_err(|error| format!("the chain did not answer: {error}"))?
            .json()
            .await
            .map_err(|error| format!("the chain said something unexpected: {error}"))?;

        if let Some(problem) = reply.get("error") {
            let message = problem
                .get("message")
                .and_then(|message| message.as_str())
                .unwrap_or("the chain refused it");
            return Err(readable(message));
        }
        Ok(reply
            .get("result")
            .and_then(|hash| hash.as_str())
            .unwrap_or_default()
            .to_string())
    }

    /// Which address the installed wallet is offering.
    async fn ask_the_browser() -> Result<String, String> {
        let provider = injected().ok_or("no browser wallet is installed")?;
        let arguments = js_sys::Object::new();
        set(
            &arguments,
            "method",
            &JsValue::from_str("eth_requestAccounts"),
        );

        let call = js_sys::Reflect::get(&provider, &JsValue::from_str("request"))
            .map_err(|_| "that wallet does not take requests".to_string())?;
        let call: js_sys::Function = call
            .dyn_into()
            .map_err(|_| "that wallet does not take requests".to_string())?;
        let answer = call
            .call1(&provider, &arguments)
            .map_err(|error| said(&error))?;
        let accounts = JsFuture::from(js_sys::Promise::from(answer))
            .await
            .map_err(|error| said(&error))?;
        let accounts = js_sys::Array::from(&accounts);
        accounts
            .get(0)
            .as_string()
            .ok_or_else(|| "that wallet offered no account".to_string())
    }

    fn injected() -> Option<JsValue> {
        let window = web_sys::window()?;
        let ethereum = js_sys::Reflect::get(&window, &JsValue::from_str("ethereum")).ok()?;
        (!ethereum.is_undefined() && !ethereum.is_null()).then_some(ethereum)
    }

    fn set(object: &js_sys::Object, key: &str, value: &JsValue) {
        let _ = js_sys::Reflect::set(object, &JsValue::from_str(key), value);
    }

    fn to_js(value: &serde_json::Value) -> Result<JsValue, String> {
        js_sys::JSON::parse(&value.to_string())
            .map_err(|_| "that transaction could not be described".to_string())
    }

    fn said(error: &JsValue) -> String {
        readable(
            &js_sys::Reflect::get(error, &JsValue::from_str("message"))
                .ok()
                .and_then(|message| message.as_string())
                .unwrap_or_else(|| "the wallet refused it".to_string()),
        )
    }

    /// A chain's revert reasons read like machine output; the ones a depositor
    /// can actually hit are worth saying properly.
    fn readable(message: &str) -> String {
        if message.contains("WrongState") {
            "This vault is not taking deposits.".to_string()
        } else if message.contains("NoStatement") || message.contains("BadSignature") {
            "The vault needs your signed statement for its list first.".to_string()
        } else if message.contains("NotAllowed") {
            "Only the agent's author can do that.".to_string()
        } else if message.contains("TooSmall") {
            "That is under the vault's minimum.".to_string()
        } else if message.contains("BelowFloor") {
            "That would take the vault's cash below its idle floor.".to_string()
        } else if message.contains("Stale") {
            "The executor's bill is out of date; try again in a moment.".to_string()
        } else if message.contains("NothingToDo") {
            "There is nothing to do here.".to_string()
        } else if message.contains("insufficient") || message.contains("Insufficient") {
            "That is more than you hold.".to_string()
        } else if message.contains("User denied") || message.contains("user rejected") {
            "You turned it down.".to_string()
        } else {
            message.to_string()
        }
    }

    use wasm_bindgen::JsCast;
}
