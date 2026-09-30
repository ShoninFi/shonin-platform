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

/// Take what the API read from the chain as the viewer's holdings.
pub fn apply(portfolio: Portfolio) {
    holdings().replace_with(|holdings| {
        holdings.balance = portfolio.balance;
        holdings.positions = portfolio.positions;
        holdings.claims = portfolio.claims;
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
    !matches!(claim.state, ClaimState::Paid)
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
    buy_review, connect, deposit, fund_tank, move_to, publish, refresh, resume, settle, withdraw,
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
    pub async fn move_to(_from: &str, _to: &str) -> Result<String, String> {
        Err(NOWHERE.to_string())
    }
    pub async fn fund_tank(_slug: &str, _assets: f64) -> Result<String, String> {
        Err(NOWHERE.to_string())
    }
    pub async fn buy_review(_slug: &str) -> Result<String, String> {
        Err(NOWHERE.to_string())
    }
    pub async fn publish(
        _id: &str,
        _version: u32,
        _tank: f64,
        _allowance: &str,
        _archive: &[u8],
        _sealed: bool,
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
    use chain::bindings::reviews::IReviewRegistry;
    use chain::bindings::vault::IVault;
    use wasm_bindgen::JsValue;
    use wasm_bindgen_futures::JsFuture;

    use super::*;
    use crate::api;

    /// Connect, and read back what this address holds.
    pub async fn connect(how: Connection, watching: Option<String>) -> Result<(), String> {
        let address = match &how {
            Connection::Browser => ask_the_browser().await?,
            Connection::Development => data::config()
                .and_then(|config| config.dev.as_ref().map(|dev| dev.address.clone()))
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
        if let Ok(portfolio) = api::portfolio(&address).await {
            apply(portfolio);
        }
    }

    /// Put money into a vault: the asset is approved, then deposited, and the
    /// contract values itself and mints at that in the same transaction.
    pub async fn deposit(slug: &str, assets: f64) -> Result<String, String> {
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

        working("Depositing");
        let hash = send(
            chain,
            &from,
            vault,
            IVault::depositCall {
                assets: amount,
                receiver: address_of(&from)?,
            }
            .abi_encode(),
        )
        .await;
        done();
        refresh().await;
        hash
    }

    /// Leave a vault. The contract pays from its idle balance if it can, and
    /// books a claim if it cannot.
    pub async fn withdraw(slug: &str, shares: f64) -> Result<String, String> {
        let (strategy, from) = about(slug)?;
        let chain = strategy.vault.chain;
        let vault = address_of(&strategy.vault.address)?;

        working("Withdrawing");
        let hash = send(
            chain,
            &from,
            vault,
            IVault::redeemCall {
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

    /// Settle a claim. Anyone may send this, which is the point of it.
    pub async fn settle(slug: &str, claim: u64) -> Result<String, String> {
        let (strategy, from) = about(slug)?;
        let chain = strategy.vault.chain;
        let vault = address_of(&strategy.vault.address)?;

        working("Settling");
        let hash = send(
            chain,
            &from,
            vault,
            IVault::settleCall {
                claimId: U256::from(claim),
            }
            .abi_encode(),
        )
        .await;
        done();
        refresh().await;
        hash
    }

    /// Move a position to a strategy's successor: a withdrawal from one vault
    /// and a deposit into the other, which is all a move can ever be.
    pub async fn move_to(from_slug: &str, to_slug: &str) -> Result<String, String> {
        let shares = holdings()
            .lock_ref()
            .position(from_slug)
            .map_or(0.0, |held| held.shares);
        if shares <= 0.0 {
            return Err("You hold nothing in this vault.".to_string());
        }
        let chain_of = |slug: &str| data::strategy(slug).map(|strategy| strategy.vault.chain);
        if chain_of(from_slug) != chain_of(to_slug) {
            return Err(
                "These vaults are on different chains, so moving between them means bringing \
                 the cash across first."
                    .to_string(),
            );
        }
        let balance_before = holdings().lock_ref().balance;
        withdraw(from_slug, shares).await?;
        let moved = holdings().lock_ref().balance - balance_before;
        if moved <= 0.0 {
            return Err(
                "More than the idle balance, so a claim was booked instead. Once it settles \
                 the cash is in your wallet, and depositing it into the new vault is a second \
                 step you take when you are ready."
                    .to_string(),
            );
        }
        deposit(to_slug, moved).await
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

    /// Publish a strategy: create its vault, funding the tank in the same
    /// transaction, then hand the source to the build service.
    ///
    /// The vault comes first because the build service will not admit a job
    /// until one exists with a funded tank. Nothing is ever compiled for free.
    pub async fn publish(
        id: &str,
        version: u32,
        tank: f64,
        allowance: &str,
        archive: &[u8],
        sealed: bool,
    ) -> Result<String, String> {
        let holdings = holdings();
        let from = {
            let held = holdings.lock_ref();
            if !held.can_sign() {
                return Err("Connect a wallet that can sign first.".to_string());
            }
            held.address.clone().ok_or("Connect a wallet first.")?
        };
        // A publish from the app creates its vault on base.
        let chain = shared::Chain::Base;
        let factory = address_of(&setup(chain)?.addresses.factory)?;
        let usd = asset_address(chain)?;
        let funding = units(tank);

        // The hash of exactly these bytes is what the vault is created with,
        // so the chain and the build service are talking about one thing.
        let source_hash = shared::manifest::hash_bytes(archive);
        let hash_bytes = shared::manifest::hash_to_bytes(&source_hash)
            .ok_or("that archive could not be hashed")?;

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

        working("Creating the vault");
        send(
            chain,
            &from,
            factory,
            IVaultFactory::createCall {
                strategyId: name_to_bytes(id),
                version,
                sourceHash: alloy_primitives::FixedBytes(hash_bytes),
                tankFunding: funding,
                // A day at the allowances the author declared, priced as the
                // platform publishes; the contract will not let a usage
                // statement charge a tank more than this in a day.
                dailyUsageCap: units(daily_cap(allowance)),
            }
            .abi_encode(),
        )
        .await?;

        working("Submitting the source");
        let accepted = api::submit(&shared::Submission {
            chain,
            strategy_id: id.to_string(),
            version,
            author: from.clone(),
            source: shared::SubmissionSource::Archive {
                base64: base64(archive),
            },
            package: id.to_string(),
            sealed,
        })
        .await?;
        done();
        Ok(accepted.job_id)
    }

    /// What a day at a shape of allowances costs at the published prices. The
    /// floor is there because a vault with no cap at all could not charge for
    /// the build that made it.
    fn daily_cap(allowance: &str) -> f64 {
        shared::Prices::published()
            .daily_cap(&shared::DeclaredAllowances::shape(allowance))
            .max(1.0)
    }

    /// The archive, as the API carries it.
    fn base64(bytes: &[u8]) -> String {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let mut buffer = [0u8; 3];
            buffer[..chunk.len()].copy_from_slice(chunk);
            let value = u32::from_be_bytes([0, buffer[0], buffer[1], buffer[2]]);
            for index in 0..4 {
                if index <= chunk.len() {
                    out.push(ALPHABET[((value >> (18 - index * 6)) & 0x3f) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    /// Buy a fresh review of a strategy. Anyone may, at any time.
    pub async fn buy_review(slug: &str) -> Result<String, String> {
        let (strategy, from) = about(slug)?;
        let chain = strategy.vault.chain;
        let setup = setup(chain)?;
        let reviews = address_of(&setup.addresses.reviews)?;
        let usd = asset_address(chain)?;
        // Approving exactly what the registry will pull, as the registry
        // itself reports it.
        let fee = units(setup.terms.review_fee);

        working("Approving");
        send(
            chain,
            &from,
            usd,
            IERC20::approveCall {
                spender: reviews,
                value: fee,
            }
            .abi_encode(),
        )
        .await?;

        working("Buying a review");
        let hash = send(
            chain,
            &from,
            reviews,
            IReviewRegistry::requestReviewCall {
                strategyId: name_to_bytes(&strategy.id),
                version: strategy.version,
            }
            .abi_encode(),
        )
        .await;
        done();
        hash
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
    fn setup(chain: shared::Chain) -> Result<shared::ChainSetup, String> {
        let config = data::config().ok_or("the app has not reached the platform yet")?;
        data::setup(&config, chain)
            .ok_or_else(|| format!("the platform does not serve {} here", chain.label()))
    }

    fn asset_address(chain: shared::Chain) -> Result<Address, String> {
        address_of(&setup(chain)?.addresses.usd)
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

    fn name_to_bytes(name: &str) -> alloy_primitives::FixedBytes<32> {
        let mut out = [0u8; 32];
        let bytes = name.as_bytes();
        let take = bytes.len().min(32);
        out[..take].copy_from_slice(&bytes[..take]);
        alloy_primitives::FixedBytes(out)
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
        match how {
            Some(Connection::Browser) => through_the_browser(request).await,
            Some(Connection::Development) => through_the_chain(chain, request).await,
            _ => Err("This connection signs nothing.".to_string()),
        }
    }

    /// Ask the wallet the viewer has installed.
    async fn through_the_browser(request: serde_json::Value) -> Result<String, String> {
        let provider = injected().ok_or("no browser wallet is installed")?;
        let arguments = js_sys::Object::new();
        set(
            &arguments,
            "method",
            &JsValue::from_str("eth_sendTransaction"),
        );
        let params = js_sys::Array::new();
        params.push(&to_js(&request)?);
        set(&arguments, "params", &params);

        let call = js_sys::Reflect::get(&provider, &JsValue::from_str("request"))
            .map_err(|_| "that wallet does not take requests".to_string())?;
        let call: js_sys::Function = call
            .dyn_into()
            .map_err(|_| "that wallet does not take requests".to_string())?;
        let answer = call
            .call1(&provider, &arguments)
            .map_err(|error| said(&error))?;
        let hash = JsFuture::from(js_sys::Promise::from(answer))
            .await
            .map_err(|error| said(&error))?;
        Ok(hash.as_string().unwrap_or_default())
    }

    /// Ask the chain, which holds the key itself.
    async fn through_the_chain(
        chain: shared::Chain,
        request: serde_json::Value,
    ) -> Result<String, String> {
        let setup = setup(chain)?;
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_sendTransaction",
            "params": [request],
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
