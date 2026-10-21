//! The page's Solana wallet session, beside the EVM one: which Solana wallet
//! is connected and its account as signals, the Solana wallets this browser
//! has, and everything that asks one something. Both kinds may be connected
//! at once; a vault's flows use the session of its chain's kind.
//!
//! - Wallets are found on load through the Wallet Standard, with Phantom's
//!   older provider as the fallback.
//! - A remembered browser or development wallet reconnects silently
//!   (`connect({ silent: true })`, `onlyIfTrusted`): never a prompt nobody
//!   asked for. Only the wallet's id is remembered, never a phrase.
//! - Account changes and disconnects are followed; either cancels every
//!   Solana flow in progress.

use std::cell::RefCell;
use std::rc::Rc;

use chain::solana_ix::Pubkey;
use chain::solana_ix::transaction::Unsigned;
use futures_signals::signal::Mutable;

use super::key::SolKey;
use super::machine::{self, Event, Refusal, SolConnection, SolWalletId};
use super::network::SolNetwork;
use super::rpc::SolRpc;
use crate::tx::flow::Cancel;
use crate::tx::solana::error::SolError;
use crate::tx::solana::port::{Signed, SolSigner};
use crate::wallet::config_cache::ConfigId;
use crate::wallet::phrase::BadPhrase;
use crate::wallet::rpc::Endpoint;

#[cfg(target_arch = "wasm32")]
use super::standard::{Legacy, StandardWallet};

/// A Solana wallet the dialog lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SolWalletOption {
    pub id: SolWalletId,
    pub name: String,
    pub icon: Option<String>,
    pub detected: bool,
    pub install: Option<&'static str>,
}

/// The Solana wallets the dialog always names, installed or not.
pub const POPULAR: [(&str, &str); 3] = [
    ("Phantom", "https://phantom.com/download"),
    ("Solflare", "https://solflare.com/download"),
    ("Backpack", "https://backpack.app/downloads"),
];

#[derive(Clone)]
enum Active {
    #[cfg(target_arch = "wasm32")]
    Standard(StandardWallet),
    #[cfg(target_arch = "wasm32")]
    Legacy(Legacy),
    /// The recovery phrase, or the development wallet: a key in the page.
    Key(SolKey),
}

pub struct SolSession {
    pub connection: Mutable<SolConnection>,
    pub wallets: Mutable<Vec<SolWalletOption>>,
    /// "Phantom", "your recovery phrase wallet".
    pub wallet_name: Mutable<String>,
    /// The Solana cluster, as the page reads it: from a copy remembered
    /// from an earlier visit at first, then from the API's answer.
    pub network: Mutable<Option<Rc<SolNetwork>>>,
    /// The cluster's config as the API wrote it, and where the page's copy
    /// came from: only a fetched copy is ever used to sign.
    config: crate::wallet::config_cache::ConfigSlot,
    /// Rises when a flow lands, so pages read the cluster again.
    pub changed: Mutable<u64>,
    active: RefCell<Option<Active>>,
    flows: RefCell<Vec<Cancel>>,
}

thread_local! {
    static SESSION: Rc<SolSession> = Rc::new(SolSession {
        connection: Mutable::new(SolConnection::SignedOut),
        wallets: Mutable::new(Vec::new()),
        wallet_name: Mutable::new(String::new()),
        network: Mutable::new(None),
        config: crate::wallet::config_cache::ConfigSlot::new(),
        changed: Mutable::new(0),
        active: RefCell::new(None),
        flows: RefCell::new(Vec::new()),
    });
}

/// The page's Solana wallet session.
pub fn get() -> Rc<SolSession> {
    SESSION.with(Rc::clone)
}

const REMEMBERED: &str = "solana-wallet";

fn remember(id: &str) {
    #[cfg(target_arch = "wasm32")]
    crate::web::storage_set(REMEMBERED, id);
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (REMEMBERED, id);
}

impl SolSession {
    fn apply(&self, event: Event) {
        let next = machine::next(&self.connection.lock_ref(), event);
        self.connection.set_neq(next);
    }

    pub fn account(&self) -> Option<Pubkey> {
        self.connection.lock_ref().account()
    }

    /// Keep a flow's cancel, so an account change stops it.
    pub fn track(&self, cancel: Cancel) {
        let mut flows = self.flows.borrow_mut();
        flows.retain(|flow| !flow.is_cancelled());
        flows.push(cancel);
    }

    fn cancel_flows(&self) {
        for flow in self.flows.borrow_mut().drain(..) {
            flow.cancel();
        }
    }

    /// Back to the list (Cancel while connecting, or after a refusal). A
    /// late approval is ignored.
    pub fn cancel_connecting(&self) {
        self.apply(Event::Cancel);
    }

    /// Disconnect, and forget the wallet.
    pub fn disconnect(&self) {
        #[cfg(target_arch = "wasm32")]
        if let Some(active) = self.active.borrow().clone() {
            wasm_bindgen_futures::spawn_local(async move {
                match active {
                    Active::Standard(wallet) => wallet.disconnect().await,
                    Active::Legacy(legacy) => legacy.disconnect().await,
                    Active::Key(_) => {}
                }
            });
        }
        self.active.replace(None);
        self.cancel_flows();
        remember("");
        self.apply(Event::Disconnected);
        self.wallet_name.set_neq(String::new());
    }

    fn connect_key(&self, id: SolWalletId, key: SolKey, name: &str) {
        let account = key.pubkey();
        if self.account().is_some() {
            self.disconnect();
        }
        self.apply(Event::Choose(id.clone()));
        self.active.replace(Some(Active::Key(key)));
        self.wallet_name.set_neq(name.to_string());
        if let Some(remembered) = id.remembered() {
            remember(&remembered);
        }
        self.apply(Event::Approved(account));
    }

    /// Connect the recovery-phrase wallet's first Solana account
    /// (`m/44'/501'/0'/0'`), held in memory only.
    pub fn connect_phrase(&self, phrase: &str) -> Result<(), BadPhrase> {
        if !crate::wallet::dev::PHRASE_WALLET {
            return Err(BadPhrase);
        }
        let key = SolKey::from_phrase(phrase, 0)?;
        self.connect_key(SolWalletId::Phrase, key, crate::wallet::PHRASE_WALLET);
        Ok(())
    }

    /// The development wallet's key on this cluster, if this build has one.
    fn dev_key(&self) -> Option<SolKey> {
        let network = self.network.get_cloned()?;
        super::dev::key(network.dev_account)
    }

    pub fn dev_available(&self) -> bool {
        self.dev_key().is_some()
    }

    /// Connect the development wallet.
    pub fn connect_dev(&self) -> bool {
        match self.dev_key() {
            Some(key) => {
                self.connect_key(SolWalletId::Dev, key, crate::wallet::DEV_WALLET);
                true
            }
            None => false,
        }
    }

    /// A reader of the cluster, through its public endpoint.
    pub fn reader(network: &SolNetwork) -> SolRpc<Endpoint> {
        SolRpc {
            transport: Endpoint {
                url: network.rpc.clone(),
            },
        }
    }

    /// The connected wallet as a flow's signer.
    pub fn signer(self: &Rc<Self>) -> SessionSigner {
        SessionSigner {
            session: self.clone(),
        }
    }

    /// The config the page reads the cluster with now: a page that drew
    /// figures with it reads again when this changes.
    pub fn config_id(&self) -> Option<ConfigId> {
        self.network
            .lock_ref()
            .as_ref()
            .map(|network| network.config)
    }

    /// Take a copy of the cluster's config into what the page reads, if it
    /// is one this build can use. Redraws only when it changed.
    fn adopt(&self, raw: &str) -> Option<Rc<SolNetwork>> {
        let network = Rc::new(parse_network(raw)?);
        if self.network.lock_ref().as_deref() != Some(&*network) {
            self.network.set(Some(network.clone()));
        }
        Some(network)
    }

    /// A copy of the cluster's config kept from an earlier visit: read from
    /// at once, never signed with.
    pub fn remember(&self, raw: &str) {
        if parse_network(raw).is_some() {
            self.config.remember(raw);
            if self.network.lock_ref().is_none() {
                self.adopt(raw);
            }
        }
    }

    /// Ask the API for the cluster's config (or wait for the ask in
    /// flight), and read from its answer.
    pub async fn refresh_with<F, Fut>(&self, fetch: F) -> Option<Rc<SolNetwork>>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<String, String>>,
    {
        let raw = self.config.refresh(fetch).await.ok()?;
        self.adopt(&raw)
    }

    /// The cluster a flow sends on: from a config fetched in this page's
    /// life, never from a remembered copy alone (see
    /// [`crate::wallet::config_cache`]), and only the one its review was
    /// computed with (`reviewed`). Any other stops the flow before a wallet
    /// is asked: the page reads the fresh one from now on, and the person
    /// reviews again.
    pub async fn network_to_sign_with<F, Fut>(
        &self,
        reviewed: ConfigId,
        fetch: F,
    ) -> Result<Rc<SolNetwork>, SolError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<String, String>>,
    {
        use crate::wallet::config_cache::ForSigning;
        let raw = match self.config.for_signing(fetch).await {
            // A fetch that replaced the remembered copy is no reason to stop
            // when the review was computed with the fetched one.
            ForSigning::Use(raw) | ForSigning::Changed(raw) => raw,
            ForSigning::Unavailable(_) => {
                return Err(SolError::Network(
                    "the Solana cluster isn't reachable from this app".to_string(),
                ));
            }
        };
        let network = self.adopt(&raw).ok_or_else(|| {
            SolError::Network("the Solana cluster's settings can't be used by this app".to_string())
        })?;
        if network.config != reviewed {
            return Err(SolError::SettingsChanged);
        }
        Ok(network)
    }

    /// The connected wallet's app, as a name on its own: "Phantom",
    /// "Development wallet".
    pub fn app_name(&self) -> String {
        match self.connection.lock_ref().wallet() {
            Some(SolWalletId::Phrase) => "Recovery phrase".to_string(),
            Some(SolWalletId::Dev) => "Development wallet".to_string(),
            _ => {
                let name = self.wallet_name.get_cloned();
                if name.is_empty() {
                    "Browser wallet".to_string()
                } else {
                    name
                }
            }
        }
    }

    /// Whether the connected wallet lives in the browser.
    pub fn is_browser(&self) -> bool {
        self.connection
            .lock_ref()
            .wallet()
            .is_some_and(SolWalletId::is_browser)
    }
}

/// The cluster a config names, if this build can use it.
fn parse_network(raw: &str) -> Option<SolNetwork> {
    SolNetwork::from_raw(raw)
}

/// The session's Solana wallet, signing.
pub struct SessionSigner {
    session: Rc<SolSession>,
}

impl SolSigner for SessionSigner {
    fn account(&self) -> Option<Pubkey> {
        self.session.account()
    }

    fn name(&self) -> String {
        self.session.wallet_name.get_cloned()
    }

    fn supports(&self, chain: &str) -> bool {
        let _ = chain;
        match self.session.active.borrow().as_ref() {
            #[cfg(target_arch = "wasm32")]
            Some(Active::Standard(wallet)) => {
                let account = self.session.account();
                wallet
                    .accounts()
                    .iter()
                    .find(|a| Some(a.address) == account)
                    // A wallet that names no chains for the account is
                    // asked anyway; one that names them must name this.
                    .is_some_and(|a| a.chains.is_empty() || a.chains.iter().any(|c| c == chain))
            }
            #[cfg(target_arch = "wasm32")]
            Some(Active::Legacy(_)) => true,
            Some(Active::Key(_)) => true,
            None => false,
        }
    }

    async fn sign(&self, unsigned: &Unsigned, chain: &str) -> Result<Signed, SolError> {
        let active = self
            .session
            .active
            .borrow()
            .clone()
            .ok_or(SolError::NotConnected)?;
        let account = self.session.account().ok_or(SolError::NotConnected)?;
        match active {
            #[cfg(target_arch = "wasm32")]
            Active::Standard(wallet) => {
                let chosen = wallet
                    .accounts()
                    .into_iter()
                    .find(|a| a.address == account)
                    .ok_or(SolError::AccountChanged)?;
                wallet.sign(&chosen, &unsigned.wire(), chain).await
            }
            #[cfg(target_arch = "wasm32")]
            Active::Legacy(legacy) => {
                // Phantom's older provider takes no chain: it sends to
                // whichever network Phantom is set to, where the blockhash
                // (read from this cluster) is unknown and nothing lands.
                // That is said as the wrong network, plainly.
                let _ = chain;
                legacy
                    .sign_and_send(&unsigned.wire())
                    .await
                    .map_err(super::legacy_error)
            }
            Active::Key(key) => {
                let _ = chain;
                if key.pubkey() != account || unsigned.signers() != [account] {
                    return Err(SolError::AccountChanged);
                }
                let signature = key.sign(unsigned.message());
                unsigned
                    .signed(&[signature])
                    .map(Signed::Wire)
                    .map_err(|_| SolError::BadSignature)
            }
        }
    }
}

// The browser ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
impl SolSession {
    /// On load: find wallets, read from the cluster's config remembered
    /// from an earlier visit, ask the API for it in the background (again,
    /// with growing waits, until it answers), and reconnect a remembered
    /// wallet if it still allows this site.
    pub fn start(self: &Rc<Self>) {
        let session = self.clone();
        super::standard::discover(Rc::new(move || session.refresh_wallets()));
        self.refresh_wallets();
        if let Some(raw) = crate::wallet::config_cache::recall(shared::Chain::Solana) {
            self.remember(&raw);
        }
        let session = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let mut failures = 0u32;
            while session.refresh().await.is_none() {
                failures = failures.saturating_add(1);
                gloo_timers::future::TimeoutFuture::new(crate::api::next_delay(failures)).await;
                if session.config.is_fresh() {
                    break;
                }
            }
        });
        let session = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            gloo_timers::future::TimeoutFuture::new(300).await;
            session.refresh_wallets();
            session.restore().await;
        });
    }

    /// Ask wallets to register again (the dialog opening).
    pub fn start_discovery(self: &Rc<Self>) {
        let session = self.clone();
        super::standard::discover(Rc::new(move || session.refresh_wallets()));
        self.refresh_wallets();
    }

    /// Ask the API for the cluster's config, keep it for the next visit if
    /// it reads well, and read from it.
    async fn refresh(&self) -> Option<Rc<SolNetwork>> {
        let network = self
            .refresh_with(|| crate::wallet::config_cache::fetch_raw(shared::Chain::Solana))
            .await?;
        if let Some(held) = self.config.held() {
            crate::wallet::config_cache::keep(shared::Chain::Solana, &held.raw);
        }
        Some(network)
    }

    /// The cluster to read from: the copy the page holds (remembered or
    /// fetched), else the API's answer.
    pub async fn load_network(&self) -> Option<Rc<SolNetwork>> {
        if let Some(network) = self.network.get_cloned() {
            return Some(network);
        }
        self.refresh().await
    }

    /// The cluster to send on: never a remembered copy alone, and only the
    /// one the review was computed with (`reviewed`).
    pub async fn network_to_sign(&self, reviewed: ConfigId) -> Result<Rc<SolNetwork>, SolError> {
        let network = self
            .network_to_sign_with(reviewed, || {
                crate::wallet::config_cache::fetch_raw(shared::Chain::Solana)
            })
            .await;
        // A fetched config is kept for the next visit, whether or not this
        // send goes on with it.
        if let Some(held) = self.config.held().filter(|_| self.config.is_fresh()) {
            crate::wallet::config_cache::keep(shared::Chain::Solana, &held.raw);
        }
        network
    }

    fn standard_named(name: &str) -> Option<StandardWallet> {
        super::standard::registered()
            .into_iter()
            .find(|wallet| wallet.name == name)
    }

    fn legacy() -> Option<Legacy> {
        // Only for a Phantom that didn't register itself.
        if Self::standard_named("Phantom").is_some() {
            return None;
        }
        Legacy::find()
    }

    fn refresh_wallets(&self) {
        let found = super::standard::registered();
        let mut options = Vec::new();
        for (name, install) in POPULAR {
            let wallet = found.iter().find(|wallet| wallet.name == name);
            let legacy = (name == "Phantom" && wallet.is_none() && Legacy::find().is_some())
                .then_some(SolWalletId::Legacy);
            options.push(SolWalletOption {
                id: legacy.unwrap_or_else(|| SolWalletId::Standard(name.to_string())),
                name: name.to_string(),
                icon: wallet.and_then(|w| w.icon.clone()),
                detected: wallet.is_some() || (name == "Phantom" && Legacy::find().is_some()),
                install: Some(install),
            });
        }
        for wallet in &found {
            if POPULAR.iter().any(|(name, _)| *name == wallet.name) {
                continue;
            }
            options.push(SolWalletOption {
                id: SolWalletId::Standard(wallet.name.clone()),
                name: wallet.name.clone(),
                icon: wallet.icon.clone(),
                detected: true,
                install: None,
            });
        }
        options.sort_by_key(|option| !option.detected);
        self.wallets.set_neq(options);
    }

    /// Connect a browser wallet: the one prompt connecting makes.
    pub async fn connect(self: &Rc<Self>, id: SolWalletId) {
        if self.account().is_some() {
            self.disconnect();
        }
        self.apply(Event::Choose(id.clone()));
        let answer = match &id {
            SolWalletId::Standard(name) => match Self::standard_named(name) {
                Some(wallet) => wallet.connect(false).await.map(|accounts| {
                    (
                        accounts.first().map(|a| a.address),
                        Active::Standard(wallet),
                    )
                }),
                None => {
                    self.apply(Event::Refused(Refusal::NotInstalled));
                    return;
                }
            },
            SolWalletId::Legacy => match Self::legacy() {
                Some(legacy) => legacy
                    .connect(false)
                    .await
                    .map(|account| (Some(account), Active::Legacy(legacy))),
                None => {
                    self.apply(Event::Refused(Refusal::NotInstalled));
                    return;
                }
            },
            SolWalletId::Phrase | SolWalletId::Dev => return,
        };
        // Cancelled meanwhile: whatever the wallet said is ignored.
        if !matches!(&*self.connection.lock_ref(), SolConnection::Connecting { wallet } if *wallet == id)
        {
            return;
        }
        match answer {
            Ok((Some(account), active)) => {
                self.listen(&active);
                let name = match &id {
                    SolWalletId::Standard(name) => name.clone(),
                    _ => "Phantom".to_string(),
                };
                self.active.replace(Some(active));
                self.wallet_name.set_neq(name);
                if let Some(remembered) = id.remembered() {
                    remember(&remembered);
                }
                self.apply(Event::Approved(account));
            }
            Ok((None, _)) => self.apply(Event::Refused(Refusal::Locked)),
            Err(SolError::Rejected) => self.apply(Event::Refused(Refusal::Rejected)),
            Err(_) => self.apply(Event::Refused(Refusal::Locked)),
        }
    }

    /// Reconnect the remembered wallet without a prompt.
    async fn restore(self: &Rc<Self>) {
        let Some(id) = crate::web::storage_get(REMEMBERED)
            .as_deref()
            .and_then(SolWalletId::from_remembered)
        else {
            return;
        };
        let found = match &id {
            SolWalletId::Dev => {
                use futures_signals::signal::SignalExt;
                // The development key comes with the cluster's config.
                self.network
                    .signal_ref(Option::is_some)
                    .wait_for(true)
                    .await;
                if self.account().is_none()
                    && matches!(*self.connection.lock_ref(), SolConnection::SignedOut)
                {
                    self.connect_dev();
                }
                return;
            }
            SolWalletId::Standard(name) => {
                // A wallet may register a moment after the page loads.
                let mut wallet = Self::standard_named(name);
                for _ in 0..10 {
                    if wallet.is_some() {
                        break;
                    }
                    gloo_timers::future::TimeoutFuture::new(200).await;
                    wallet = Self::standard_named(name);
                }
                let Some(wallet) = wallet else { return };
                match wallet.connect(true).await {
                    Ok(accounts) => accounts
                        .first()
                        .map(|a| (a.address, Active::Standard(wallet.clone()), name.clone())),
                    Err(_) => None,
                }
            }
            SolWalletId::Legacy => {
                let Some(legacy) = Self::legacy() else { return };
                legacy
                    .connect(true)
                    .await
                    .ok()
                    .map(|account| (account, Active::Legacy(legacy), "Phantom".to_string()))
            }
            SolWalletId::Phrase => None,
        };
        let Some((account, active, name)) = found else {
            return;
        };
        if self.account().is_some()
            || !matches!(*self.connection.lock_ref(), SolConnection::SignedOut)
        {
            return;
        }
        self.listen(&active);
        self.active.replace(Some(active));
        self.wallet_name.set_neq(name);
        self.apply(Event::Restored {
            wallet: id,
            account,
        });
    }

    /// Follow the wallet's own events, once per wallet.
    fn listen(self: &Rc<Self>, active: &Active) {
        thread_local! {
            static HEARD: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
        }
        let tag = match active {
            Active::Standard(wallet) => format!("standard:{}", wallet.name),
            Active::Legacy(_) => "legacy".to_string(),
            Active::Key(_) => return,
        };
        let new = HEARD.with(|heard| {
            let mut heard = heard.borrow_mut();
            if heard.contains(&tag) {
                false
            } else {
                heard.push(tag.clone());
                true
            }
        });
        if !new {
            return;
        }
        let session = Rc::downgrade(self);
        let changed = move |account: Option<Pubkey>, which: &str| {
            let Some(session) = session.upgrade() else {
                return;
            };
            let active = match session.active.borrow().as_ref() {
                Some(Active::Standard(wallet)) => format!("standard:{}", wallet.name),
                Some(Active::Legacy(_)) => "legacy".to_string(),
                _ => String::new(),
            };
            if active != which {
                return;
            }
            if account != session.account() {
                // A flow built for the old account must not go on.
                session.cancel_flows();
            }
            if account.is_none() {
                session.active.replace(None);
            }
            session.apply(Event::AccountChanged(account));
        };
        match active {
            Active::Standard(wallet) => {
                let watched = wallet.clone();
                let tag = tag.clone();
                wallet.on_change(move |_| {
                    let accounts = watched.accounts();
                    changed(accounts.first().map(|a| a.address), &tag);
                });
            }
            Active::Legacy(legacy) => {
                let on_change = Rc::new(changed);
                let account_changed = on_change.clone();
                legacy.on("accountChanged", move |account| {
                    account_changed(account, "legacy");
                });
                legacy.on("disconnect", move |_| on_change(None, "legacy"));
            }
            Active::Key(_) => {}
        }
    }
}

// Without a browser ---------------------------------------------------------------------

#[cfg(not(target_arch = "wasm32"))]
impl SolSession {
    pub fn start(self: &Rc<Self>) {}

    pub async fn load_network(&self) -> Option<Rc<SolNetwork>> {
        self.network.get_cloned()
    }

    pub async fn connect(self: &Rc<Self>, id: SolWalletId) {
        self.apply(Event::Choose(id));
        self.apply(Event::Refused(Refusal::NotInstalled));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A devnet config naming this build's programs, as the API writes it.
    fn config_text() -> String {
        use chain::solana_ix::ids;
        serde_json::json!({
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
                "mint": Pubkey::new_from_array([7; 32]).to_string(),
                "token_program": ids::TOKEN.to_string(),
                "vault_factory": Pubkey::new_from_array([1; 32]).to_string(),
                "publishing_config": Pubkey::new_from_array([2; 32]).to_string(),
            },
            "asset_symbol": "USDC",
            "asset_decimals": 6,
            "terms": {
                "minimum_tank": 10.0, "publish_fee": 1.0,
                "fees": {"holding_bps": 100, "author_bps": 1000, "platform_bps": 500, "transaction_bps": 10},
                "exit_band_bps": 100, "freeze_after_days": 7.0, "image_timelock_days": 2.0,
                "minimum_deposit": 10.0, "terms_hash": format!("0x{}", "00".repeat(32)),
            },
            "dev": null,
        })
        .to_string()
    }

    fn fresh_session() -> SolSession {
        SolSession {
            connection: Mutable::new(SolConnection::SignedOut),
            wallets: Mutable::new(Vec::new()),
            wallet_name: Mutable::new(String::new()),
            network: Mutable::new(None),
            config: crate::wallet::config_cache::ConfigSlot::new(),
            changed: Mutable::new(0),
            active: RefCell::new(None),
            flows: RefCell::new(Vec::new()),
        }
    }

    #[test]
    fn a_remembered_cluster_config_is_read_from_at_once_but_never_signed_with_until_the_api_agrees()
    {
        use futures::executor::block_on;
        // As the page keeps it: without the chain's head, and still read.
        let raw = crate::wallet::config_cache::canonical(&config_text()).unwrap();
        let session = fresh_session();
        session.remember(&raw);
        // Readers have the cluster at once, from the remembered copy.
        let read = session
            .network
            .get_cloned()
            .expect("read from the remembered copy");
        assert_eq!(read.cluster, "devnet");
        // A send while the API is down: nothing is signed.
        let down = || async { Err::<String, String>("the API answered 503".to_string()) };
        assert!(matches!(
            block_on(session.network_to_sign_with(read.config, down)),
            Err(SolError::Network(_))
        ));
        // The API answers the same bytes: the send goes on, with it.
        let same = raw.clone();
        let signed =
            block_on(session.network_to_sign_with(read.config, || async move { Ok(same) }))
                .unwrap();
        assert_eq!(*signed, *read);

        // The API answers something else: this send stops, and the page
        // reads the fresh config from now on.
        let session = fresh_session();
        session.remember(&raw);
        let other = raw.replace(
            "https://api.devnet.solana.com",
            "https://devnet.example.com",
        );
        let fresh = other.clone();
        assert!(matches!(
            block_on(session.network_to_sign_with(read.config, || async move { Ok(fresh) })),
            Err(SolError::SettingsChanged)
        ));
        assert_eq!(
            session.network.get_cloned().unwrap().rpc,
            "https://devnet.example.com"
        );
        // A copy that names another deployment's programs is never read.
        let session = fresh_session();
        session.remember(&raw.replace(
            &chain::solana_ix::ids::VAULT.to_string(),
            &Pubkey::new_from_array([9; 32]).to_string(),
        ));
        assert!(session.network.get_cloned().is_none());
    }

    #[test]
    fn the_phrase_wallet_connects_in_memory_signs_only_for_its_account_and_is_never_remembered() {
        let session = get();
        session
            .connect_phrase("test test test test test test test test test test test junk")
            .unwrap();
        let account = session.account().unwrap();
        assert!(!session.is_browser());
        let signer = session.signer();
        assert!(signer.supports("solana:devnet"));
        // A transaction for this account is signed; one paid by another key
        // is refused.
        let hash = chain::solana_ix::Hash::new_from_array([7; 32]);
        let ix = chain::solana_ix::vault::retire(&chain::solana_ix::vault::VaultKeys::new(
            1,
            Pubkey::new_from_array([2; 32]),
            account,
            Pubkey::new_from_array([3; 32]),
        ));
        let unsigned = Unsigned::legacy(&account, std::slice::from_ref(&ix), hash).unwrap();
        let signed = futures::executor::block_on(signer.sign(&unsigned, "solana:devnet")).unwrap();
        let Signed::Wire(wire) = signed else {
            panic!("a key in the page hands the transaction back");
        };
        assert_eq!(
            crate::tx::solana::flow::check_signed(&wire, &unsigned, &account).map(|_| ()),
            Ok(())
        );
        let other = Pubkey::new_from_array([9; 32]);
        let theirs = Unsigned::legacy(&other, &[ix], hash).unwrap();
        assert_eq!(
            futures::executor::block_on(signer.sign(&theirs, "solana:devnet")),
            Err(SolError::AccountChanged)
        );
        session.disconnect();
        assert_eq!(*session.connection.lock_ref(), SolConnection::SignedOut);
        assert!(session.connect_phrase("not a phrase").is_err());
    }
}
