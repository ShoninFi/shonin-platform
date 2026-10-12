//! The page's one wallet session: which wallet is connected, its account and
//! chain as signals, the wallets this browser has, and everything that asks
//! a wallet something. The connection's transitions are
//! [`machine::next`](super::machine::next); this feeds it.
//!
//! - Wallets are found on load (EIP-6963 announcements, which arrive
//!   asynchronously), with `window.ethereum` as the fallback.
//! - A remembered browser or development wallet reconnects silently with
//!   `eth_accounts`: never a prompt nobody asked for. Only the wallet's id is
//!   remembered, never a phrase.
//! - `accountsChanged`, `chainChanged` and `disconnect` are followed; an
//!   account change or a disconnect cancels every flow in progress.
//! - Switching chain asks `wallet_switchEthereumChain`, then
//!   `wallet_addEthereumChain` with the public endpoint and explorer if the
//!   wallet doesn't know the chain.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use alloy_primitives::{Address, B256, Bytes};
use futures_signals::signal::Mutable;
use serde_json::{Value, json};
use shared::Chain;

use super::dev;
use super::machine::{self, Connection, Event, Refusal, WalletId};
use super::network::Network;
use super::phrase::{BadPhrase, Fees, LocalKey, Unsigned};
use super::rpc::{Endpoint, RpcReader, transaction_json};
#[cfg(target_arch = "wasm32")]
use super::{
    networks,
    rpc::{Transport, quantity_u64},
};
use crate::tx::error::TxError;
use crate::tx::flow::Cancel;
use crate::tx::port::{Reader, Signer, StatementRequest, TxRequest};

#[cfg(target_arch = "wasm32")]
use super::eip1193::{self, Announced, Provider};

/// A wallet the dialog lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalletOption {
    pub id: WalletId,
    pub name: String,
    /// A `data:image/…` icon the wallet announced, if any.
    pub icon: Option<String>,
    /// Installed in this browser.
    pub detected: bool,
    /// Where to get it, when it isn't.
    pub install: Option<&'static str>,
}

/// The wallets the dialog always names, installed or not.
pub const POPULAR: [(&str, &str, &str); 4] = [
    ("io.metamask", "MetaMask", "https://metamask.io/download/"),
    ("app.phantom", "Phantom", "https://phantom.com/download"),
    ("io.rabby", "Rabby", "https://rabby.io/"),
    (
        "com.coinbase.wallet",
        "Coinbase Wallet",
        "https://www.coinbase.com/wallet/downloads",
    ),
];

/// The wallet that is connected.
#[derive(Clone)]
enum Active {
    #[cfg(target_arch = "wasm32")]
    Browser(Announced),
    Phrase(LocalKey),
    /// A development chain's own account: the chain signs.
    DevNode(Address),
    /// The test wallet's phrase, compiled into a development build.
    DevPhrase(LocalKey),
}

pub struct Session {
    pub connection: Mutable<Connection>,
    /// The wallets for the dialog, refreshed as wallets announce themselves.
    pub wallets: Mutable<Vec<WalletOption>>,
    /// The connected wallet's name, for "Approve in MetaMask".
    pub wallet_name: Mutable<String>,
    /// Vault chains, as their configs are read.
    pub networks: Mutable<BTreeMap<Chain, Rc<Network>>>,
    /// Every vault by slug, from the latest snapshot this session read.
    pub vaults: Mutable<BTreeMap<String, Rc<super::run::VaultRef>>>,
    /// Rises when a flow lands, so pages read the chain again.
    pub changed: Mutable<u64>,
    /// When the vaults were last read, on the reader's clock.
    vaults_at: std::cell::Cell<f64>,
    active: RefCell<Option<Active>>,
    /// Flows in progress: cancelled when the account changes.
    flows: RefCell<Vec<Cancel>>,
}

thread_local! {
    static SESSION: Rc<Session> = Rc::new(Session {
        connection: Mutable::new(Connection::SignedOut),
        wallets: Mutable::new(Vec::new()),
        wallet_name: Mutable::new(String::new()),
        networks: Mutable::new(BTreeMap::new()),
        vaults: Mutable::new(BTreeMap::new()),
        changed: Mutable::new(0),
        vaults_at: std::cell::Cell::new(f64::NEG_INFINITY),
        active: RefCell::new(None),
        flows: RefCell::new(Vec::new()),
    });
}

/// The page's wallet session.
pub fn get() -> Rc<Session> {
    SESSION.with(Rc::clone)
}

const REMEMBERED: &str = "wallet";

/// Keep the wallet's id (never anything else) for a silent reconnect.
fn remember(id: &str) {
    #[cfg(target_arch = "wasm32")]
    crate::web::storage_set(REMEMBERED, id);
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (REMEMBERED, id);
}

impl Session {
    fn apply(&self, event: Event) {
        let next = machine::next(&self.connection.lock_ref(), event);
        self.connection.set_neq(next);
    }

    pub fn account(&self) -> Option<Address> {
        self.connection.lock_ref().account()
    }

    /// The chain a browser wallet is on, as it last said.
    pub fn wallet_chain(&self) -> Option<u64> {
        match &*self.connection.lock_ref() {
            Connection::Connected { chain_id, .. } => *chain_id,
            _ => None,
        }
    }

    /// Whether the connected wallet lives in the browser (and so can be on
    /// the wrong chain). A local signer signs for whichever chain it's asked.
    pub fn is_browser(&self) -> bool {
        self.connection
            .lock_ref()
            .wallet()
            .is_some_and(WalletId::is_browser)
    }

    /// Whether this build and these chains offer the development wallet.
    pub fn dev_available(&self) -> bool {
        self.dev_account().is_some()
    }

    fn dev_account(&self) -> Option<Active> {
        if let Some(account) = self
            .networks
            .lock_ref()
            .values()
            .find_map(|network| network.dev_account)
        {
            return Some(Active::DevNode(account));
        }
        dev::PHRASE
            .filter(|phrase| !phrase.trim().is_empty())
            .and_then(|phrase| LocalKey::from_phrase(phrase).ok())
            .map(Active::DevPhrase)
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

    pub fn open(&self) {
        self.apply(Event::Open);
    }

    pub fn close(&self) {
        self.apply(Event::Close);
    }

    /// "Cancel" while connecting: back to the list. A late approval from the
    /// wallet is ignored.
    pub fn cancel_connecting(&self) {
        self.apply(Event::Cancel);
    }

    /// Disconnect, and forget the wallet.
    pub fn disconnect(&self) {
        self.active.replace(None);
        self.cancel_flows();
        remember("");
        self.apply(Event::Disconnected);
        self.wallet_name.set_neq(String::new());
    }

    /// Connect with a recovery phrase, held in memory only.
    pub fn connect_phrase(&self, phrase: &str) -> Result<(), BadPhrase> {
        if !dev::PHRASE_WALLET {
            return Err(BadPhrase);
        }
        let key = LocalKey::from_phrase(phrase)?;
        let account = key.address();
        self.apply(Event::Choose(WalletId::Phrase));
        self.active.replace(Some(Active::Phrase(key)));
        self.wallet_name
            .set_neq("your recovery phrase wallet".to_string());
        self.apply(Event::Approved {
            account,
            chain_id: None,
        });
        Ok(())
    }

    /// Connect the development wallet.
    pub fn connect_dev(&self) -> bool {
        let Some(active) = self.dev_account() else {
            return false;
        };
        let account = match &active {
            Active::DevNode(account) => *account,
            Active::DevPhrase(key) => key.address(),
            _ => return false,
        };
        self.apply(Event::Choose(WalletId::Dev));
        self.active.replace(Some(active));
        self.wallet_name
            .set_neq("the development wallet".to_string());
        remember("dev");
        self.apply(Event::Approved {
            account,
            chain_id: None,
        });
        true
    }

    /// A reader of a vault chain, through its public endpoint.
    pub fn reader(network: &Network) -> RpcReader<Endpoint> {
        RpcReader {
            transport: Endpoint {
                url: network.rpc.clone(),
            },
        }
    }

    /// The connected wallet as a flow's signer, for one vault chain.
    pub fn signer(self: &Rc<Self>, network: Rc<Network>) -> SessionSigner {
        SessionSigner {
            session: self.clone(),
            network,
        }
    }
}

/// The session's wallet, signing for one vault chain.
pub struct SessionSigner {
    session: Rc<Session>,
    network: Rc<Network>,
}

impl SessionSigner {
    fn active(&self) -> Result<Active, TxError> {
        self.session
            .active
            .borrow()
            .clone()
            .ok_or(TxError::NotConnected)
    }

    fn endpoint(&self) -> RpcReader<Endpoint> {
        Session::reader(&self.network)
    }

    /// The endpoint answers for the chain the transaction is signed for.
    async fn check_endpoint(&self, chain_id: u64) -> Result<RpcReader<Endpoint>, TxError> {
        let reader = self.endpoint();
        let endpoint = reader.chain_id().await?;
        if endpoint != chain_id {
            return Err(TxError::WrongEndpoint {
                endpoint,
                vault: chain_id,
            });
        }
        Ok(reader)
    }

    /// Sign locally and send through the public endpoint.
    async fn send_local(&self, key: &LocalKey, request: &TxRequest) -> Result<B256, TxError> {
        if key.address() != request.from {
            return Err(TxError::AccountChanged);
        }
        let reader = self.check_endpoint(request.chain_id).await?;
        let nonce = reader.pending_nonce(request.from).await?;
        let gas = match request.gas {
            Some(gas) => gas,
            None => reader.estimate_gas(request).await?,
        };
        let tip = reader.priority_fee().await;
        let fees = match reader.base_fee().await? {
            Some(base) => {
                let tip = u128::try_from(tip).unwrap_or(0);
                let base = u128::try_from(base).unwrap_or(u128::MAX / 4);
                Fees::Eip1559 {
                    max_fee: base.saturating_mul(2).saturating_add(tip),
                    max_priority_fee: tip,
                }
            }
            None => Fees::Legacy {
                gas_price: u128::try_from(reader.gas_price().await?).unwrap_or(u128::MAX),
            },
        };
        let raw = key.sign_transaction(&Unsigned {
            chain_id: request.chain_id,
            nonce,
            gas,
            fees,
            to: request.to,
            data: request.data.clone(),
        });
        reader.send_raw(&raw).await
    }
}

fn hash_of(value: &Value) -> Result<B256, TxError> {
    value
        .as_str()
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| TxError::Wallet("it answered without a transaction hash".to_string()))
}

fn signature_of(value: &Value) -> Result<Bytes, TxError> {
    let text = value
        .as_str()
        .ok_or_else(|| TxError::Wallet("it answered without a signature".to_string()))?;
    super::rpc::hex_bytes(text).map(Bytes::from)
}

impl Signer for SessionSigner {
    fn account(&self) -> Option<Address> {
        self.session.account()
    }

    fn name(&self) -> String {
        self.session.wallet_name.get_cloned()
    }

    async fn chain_id(&self) -> Result<u64, TxError> {
        match self.active()? {
            #[cfg(target_arch = "wasm32")]
            Active::Browser(wallet) => {
                let answer = wallet
                    .provider
                    .request("eth_chainId", json!([]))
                    .await
                    .map_err(super::rpc::RpcError::into_tx_error)?;
                quantity_u64(&answer)
            }
            // A local signer signs for the chain it is asked to, and checks
            // the endpoint answers for it before it sends.
            Active::Phrase(_) | Active::DevNode(_) | Active::DevPhrase(_) => {
                Ok(self.network.chain_id)
            }
        }
    }

    async fn sign_statement(&self, statement: &StatementRequest) -> Result<Bytes, TxError> {
        match self.active()? {
            #[cfg(target_arch = "wasm32")]
            Active::Browser(wallet) => {
                let params = json!([
                    format!("{:?}", statement.depositor),
                    statement.typed_data().to_string()
                ]);
                let answer = wallet
                    .provider
                    .request("eth_signTypedData_v4", params)
                    .await
                    .map_err(super::rpc::RpcError::into_tx_error)?;
                signature_of(&answer)
            }
            Active::Phrase(key) | Active::DevPhrase(key) => {
                if key.address() != statement.depositor {
                    return Err(TxError::AccountChanged);
                }
                Ok(key.sign_digest(statement.digest()))
            }
            Active::DevNode(account) => {
                let reader = self.check_endpoint(statement.chain_id).await?;
                let params = json!([format!("{account:?}"), statement.typed_data()]);
                signature_of(&reader.ask("eth_signTypedData_v4", params).await?)
            }
        }
    }

    async fn send(&self, request: &TxRequest) -> Result<B256, TxError> {
        match self.active()? {
            #[cfg(target_arch = "wasm32")]
            Active::Browser(wallet) => {
                let answer = wallet
                    .provider
                    .request("eth_sendTransaction", json!([transaction_json(request)]))
                    .await
                    .map_err(super::rpc::RpcError::into_tx_error)?;
                hash_of(&answer)
            }
            Active::Phrase(key) | Active::DevPhrase(key) => self.send_local(&key, request).await,
            Active::DevNode(account) => {
                if account != request.from {
                    return Err(TxError::AccountChanged);
                }
                let reader = self.check_endpoint(request.chain_id).await?;
                hash_of(
                    &reader
                        .ask("eth_sendTransaction", json!([transaction_json(request)]))
                        .await?,
                )
            }
        }
    }
}

// The browser ---------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
impl Session {
    /// On load: find wallets, read the vault chains' configs, and reconnect
    /// a remembered wallet if it still allows this site.
    pub fn start(self: &Rc<Self>) {
        let session = self.clone();
        eip1193::discover(Rc::new(move || session.refresh_wallets()));
        self.refresh_wallets();
        let session = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            session.load_networks().await;
            // Wallets announce within a moment of being asked.
            gloo_timers::future::TimeoutFuture::new(300).await;
            session.refresh_wallets();
            session.restore().await;
        });
    }

    /// Ask every wallet to announce itself again (the dialog opening), and
    /// list what answered.
    pub fn start_discovery(self: &Rc<Self>) {
        let session = self.clone();
        eip1193::discover(Rc::new(move || session.refresh_wallets()));
        self.refresh_wallets();
    }

    /// Read every vault chain's config the API serves.
    pub async fn load_networks(&self) {
        let Ok(snapshot) = fetch::<shared::Snapshot>("snapshot").await else {
            return;
        };
        self.vaults.set(super::run::directory(&snapshot));
        self.vaults_at.set(js_sys::Date::now());
        for info in snapshot.chains {
            if self.networks.lock_ref().contains_key(&info.chain) {
                continue;
            }
            if let Ok(config) = fetch::<shared::Config>(&format!("{}/config", info.chain)).await
                && let Ok(network) = Network::from_config(&config)
            {
                self.networks
                    .lock_mut()
                    .insert(info.chain, Rc::new(network));
            }
        }
        self.refresh_wallets();
    }

    /// Every vault, read again if the copy held is more than 15 s old.
    pub async fn read_vaults(&self) -> BTreeMap<String, Rc<super::run::VaultRef>> {
        if js_sys::Date::now() - self.vaults_at.get() > 15_000.0
            && let Ok(snapshot) = fetch::<shared::Snapshot>("snapshot").await
        {
            self.vaults.set(super::run::directory(&snapshot));
            self.vaults_at.set(js_sys::Date::now());
        }
        self.vaults.get_cloned()
    }

    /// A vault chain's network, reading its config if it hasn't been.
    pub async fn network(&self, chain: Chain) -> Option<Rc<Network>> {
        if let Some(network) = self.networks.lock_ref().get(&chain) {
            return Some(network.clone());
        }
        let config = fetch::<shared::Config>(&format!("{chain}/config"))
            .await
            .ok()?;
        let network = Rc::new(Network::from_config(&config).ok()?);
        self.networks.lock_mut().insert(chain, network.clone());
        Some(network)
    }

    fn browser_wallets() -> Vec<Announced> {
        let mut found = eip1193::announced();
        if found.is_empty()
            && let Some(legacy) = eip1193::legacy()
        {
            found.push(legacy);
        }
        found
    }

    fn refresh_wallets(&self) {
        let found = Self::browser_wallets();
        let mut options: Vec<WalletOption> = Vec::new();
        // The popular ones first, detected or not, then any other wallet that
        // announced itself.
        for (rdns, name, install) in POPULAR {
            let wallet = found.iter().find(|wallet| wallet.rdns == rdns);
            options.push(WalletOption {
                id: WalletId::Announced(rdns.to_string()),
                name: wallet
                    .map(|w| w.name.clone())
                    .unwrap_or_else(|| name.to_string()),
                icon: wallet.and_then(|w| w.icon.clone()),
                detected: wallet.is_some(),
                install: Some(install),
            });
        }
        for wallet in &found {
            if POPULAR.iter().any(|(rdns, ..)| *rdns == wallet.rdns) {
                continue;
            }
            options.push(WalletOption {
                id: if wallet.rdns.is_empty() {
                    WalletId::Injected
                } else {
                    WalletId::Announced(wallet.rdns.clone())
                },
                name: wallet.name.clone(),
                icon: wallet.icon.clone(),
                detected: true,
                install: None,
            });
        }
        // Detected wallets first, in the same order otherwise.
        options.sort_by_key(|option| !option.detected);
        self.wallets.set(options);
    }

    fn find(&self, id: &WalletId) -> Option<Announced> {
        let found = Self::browser_wallets();
        match id {
            WalletId::Announced(rdns) => found.into_iter().find(|wallet| &wallet.rdns == rdns),
            WalletId::Injected => found.into_iter().find(|wallet| wallet.rdns.is_empty()),
            _ => None,
        }
    }

    /// Connect a browser wallet: asks it for an account, which is the one
    /// prompt connecting makes.
    pub async fn connect(self: &Rc<Self>, id: WalletId) {
        self.apply(Event::Choose(id.clone()));
        let Some(wallet) = self.find(&id) else {
            self.apply(Event::Refused(Refusal::NotInstalled));
            return;
        };
        let answer = wallet
            .provider
            .request("eth_requestAccounts", json!([]))
            .await;
        // Cancelled meanwhile: whatever the wallet said is ignored.
        if self.connection.lock_ref().wallet() != Some(&id)
            || !matches!(*self.connection.lock_ref(), Connection::Connecting { .. })
        {
            return;
        }
        let account = match answer {
            Ok(accounts) => first_account(&accounts),
            Err(error) => {
                let refusal = match error.into_tx_error() {
                    TxError::Rejected => Refusal::Rejected,
                    _ => Refusal::Locked,
                };
                self.apply(Event::Refused(refusal));
                return;
            }
        };
        let Some(account) = account else {
            self.apply(Event::Refused(Refusal::Locked));
            return;
        };
        let chain_id = ask_chain(&wallet.provider).await;
        self.listen(&wallet);
        self.active.replace(Some(Active::Browser(wallet.clone())));
        self.wallet_name.set_neq(wallet.name.clone());
        if let Some(remembered) = id.remembered() {
            remember(&remembered);
        }
        self.apply(Event::Approved { account, chain_id });
    }

    /// Reconnect the remembered wallet without a prompt: `eth_accounts`
    /// answers only if the wallet still allows this site.
    async fn restore(self: &Rc<Self>) {
        let Some(id) = crate::web::storage_get(REMEMBERED)
            .as_deref()
            .and_then(WalletId::from_remembered)
        else {
            return;
        };
        if id == WalletId::Dev {
            if matches!(*self.connection.lock_ref(), Connection::SignedOut) {
                self.connect_dev();
            }
            return;
        }
        let Some(wallet) = self.find(&id) else {
            return;
        };
        let Ok(accounts) = wallet.provider.request("eth_accounts", json!([])).await else {
            return;
        };
        let Some(account) = first_account(&accounts) else {
            return;
        };
        if !matches!(*self.connection.lock_ref(), Connection::SignedOut) {
            return;
        }
        let chain_id = ask_chain(&wallet.provider).await;
        self.listen(&wallet);
        self.active.replace(Some(Active::Browser(wallet.clone())));
        self.wallet_name.set_neq(wallet.name.clone());
        self.apply(Event::Restored {
            wallet: id,
            account,
            chain_id,
        });
    }

    /// Follow the wallet's own events, once per wallet.
    fn listen(self: &Rc<Self>, wallet: &Announced) {
        thread_local! {
            static HEARD: RefCell<Vec<Provider>> = const { RefCell::new(Vec::new()) };
        }
        let new = HEARD.with(|heard| {
            let mut heard = heard.borrow_mut();
            if heard.contains(&wallet.provider) {
                false
            } else {
                heard.push(wallet.provider.clone());
                true
            }
        });
        if !new {
            return;
        }
        let is_active = {
            let provider = wallet.provider.clone();
            move |session: &Session| matches!(&*session.active.borrow(), Some(Active::Browser(active)) if active.provider == provider)
        };

        let session = Rc::downgrade(self);
        let active = is_active.clone();
        wallet.provider.on("accountsChanged", move |accounts| {
            let Some(session) = session.upgrade() else {
                return;
            };
            if !active(&session) {
                return;
            }
            let accounts = serde_json::from_str::<Value>(
                &js_sys::JSON::stringify(&accounts)
                    .ok()
                    .and_then(|text| text.as_string())
                    .unwrap_or_default(),
            )
            .unwrap_or(Value::Null);
            let account = first_account(&accounts);
            if account != session.account() {
                // A flow built for the old account must not go on.
                session.cancel_flows();
            }
            if account.is_none() {
                session.active.replace(None);
            }
            session.apply(Event::AccountsChanged(account));
        });

        let session = Rc::downgrade(self);
        let active = is_active.clone();
        wallet.provider.on("chainChanged", move |chain| {
            let Some(session) = session.upgrade() else {
                return;
            };
            if !active(&session) {
                return;
            }
            if let Some(chain_id) = chain
                .as_string()
                .and_then(|hex| u64::from_str_radix(hex.trim_start_matches("0x"), 16).ok())
            {
                session.apply(Event::ChainChanged(chain_id));
            }
        });

        let session = Rc::downgrade(self);
        wallet.provider.on("disconnect", move |_| {
            let Some(session) = session.upgrade() else {
                return;
            };
            if !is_active(&session) {
                return;
            }
            session.cancel_flows();
            session.active.replace(None);
            session.apply(Event::Disconnected);
        });
    }

    /// Put the wallet on a vault's chain: switch, or add it first if the
    /// wallet doesn't know it. A local signer is on every chain already.
    pub async fn switch_to(&self, network: &Network) -> Result<(), TxError> {
        let wallet = match self.active.borrow().clone() {
            Some(Active::Browser(wallet)) => wallet,
            Some(_) => return Ok(()),
            None => return Err(TxError::NotConnected),
        };
        let switch = json!([{"chainId": networks::hex_id(network.chain_id)}]);
        if let Err(error) = wallet
            .provider
            .request("wallet_switchEthereumChain", switch.clone())
            .await
        {
            let unknown = error.code == 4902
                || error.message.contains("Unrecognized chain")
                || error.message.contains("wallet_addEthereumChain");
            if !unknown {
                return Err(error.into_tx_error());
            }
            if !networks::is_usable_endpoint(&network.rpc) {
                return Err(TxError::Network(
                    "this chain has no public endpoint to add".to_string(),
                ));
            }
            let add = networks::add_chain_params(network.chain_id, &network.label, &network.rpc);
            wallet
                .provider
                .request("wallet_addEthereumChain", json!([add]))
                .await
                .map_err(super::rpc::RpcError::into_tx_error)?;
            // Some wallets add without switching.
            let _ = wallet
                .provider
                .request("wallet_switchEthereumChain", switch)
                .await;
        }
        let now = ask_chain(&wallet.provider).await;
        if let Some(chain_id) = now {
            self.apply(Event::ChainChanged(chain_id));
        }
        if now == Some(network.chain_id) {
            Ok(())
        } else {
            Err(TxError::WrongNetwork {
                wallet: now.unwrap_or_default(),
                vault: network.chain_id,
            })
        }
    }
}

#[cfg(target_arch = "wasm32")]
async fn ask_chain(provider: &Provider) -> Option<u64> {
    let answer = provider.request("eth_chainId", json!([])).await.ok()?;
    quantity_u64(&answer).ok()
}

/// The account a wallet's list offers first: the one it has selected.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
fn first_account(accounts: &Value) -> Option<Address> {
    accounts.as_array()?.first()?.as_str()?.parse().ok()
}

#[cfg(target_arch = "wasm32")]
async fn fetch<T: serde::de::DeserializeOwned>(what: &str) -> Result<T, String> {
    let reply = gloo_net::http::Request::get(&format!("{}/{what}", crate::api::BASE))
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !reply.ok() {
        return Err(format!("the API answered {}", reply.status()));
    }
    reply.json().await.map_err(|error| error.to_string())
}

// Without a browser ---------------------------------------------------------

#[cfg(not(target_arch = "wasm32"))]
impl Session {
    /// Nothing to start: the pre-render connects no wallet.
    pub fn start(self: &Rc<Self>) {}

    pub async fn network(&self, chain: Chain) -> Option<Rc<Network>> {
        self.networks.lock_ref().get(&chain).cloned()
    }

    pub async fn read_vaults(&self) -> BTreeMap<String, Rc<super::run::VaultRef>> {
        let _ = &self.vaults_at;
        self.vaults.get_cloned()
    }

    pub async fn connect(self: &Rc<Self>, _id: WalletId) {
        self.apply(Event::Choose(WalletId::Injected));
        self.apply(Event::Refused(Refusal::NotInstalled));
    }

    pub async fn switch_to(&self, _network: &Network) -> Result<(), TxError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_account_a_wallet_offers_is_the_one_used() {
        let accounts = json!([
            format!("0x{}", "ab".repeat(20)),
            format!("0x{}", "cd".repeat(20))
        ]);
        assert_eq!(first_account(&accounts), Some(Address::repeat_byte(0xab)));
        assert_eq!(first_account(&json!([])), None);
        assert_eq!(first_account(&json!(["nonsense"])), None);
        assert_eq!(first_account(&Value::Null), None);
    }

    #[test]
    fn the_phrase_wallet_connects_in_memory_and_is_never_remembered() {
        let session = get();
        session
            .connect_phrase("test test test test test test test test test test test junk")
            .unwrap();
        assert!(matches!(
            &*session.connection.lock_ref(),
            Connection::Connected {
                wallet: WalletId::Phrase,
                ..
            }
        ));
        assert!(!session.is_browser());
        session.disconnect();
        assert_eq!(*session.connection.lock_ref(), Connection::SignedOut);
        assert!(session.connect_phrase("not a phrase").is_err());
    }
}
