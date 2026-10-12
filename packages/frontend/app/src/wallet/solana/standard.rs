//! Solana browser wallets: the Wallet Standard (Phantom, Solflare, Backpack
//! and every wallet that registers itself), with Phantom's older injected
//! provider (`window.phantom.solana`) as the fallback. This is the only
//! place the app touches a Solana wallet's JavaScript, through `js_sys`
//! alone: no npm bundle, and nothing evaluated from text.
//!
//! Discovery follows the standard's handshake: the app listens for
//! `wallet-standard:register-wallet` (a wallet loaded after the app) and
//! dispatches `wallet-standard:app-ready` (for wallets already loaded), each
//! carrying an object whose `register(wallet)` the wallet calls.

use std::cell::RefCell;
use std::rc::Rc;

use chain::solana_ix::Pubkey;
use js_sys::{Array, Function, Object, Promise, Reflect, Uint8Array};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

use crate::tx::solana::error::SolError;
use crate::tx::solana::port::{Sig, Signed};

fn get(target: &JsValue, key: &str) -> JsValue {
    Reflect::get(target, &JsValue::from_str(key)).unwrap_or(JsValue::UNDEFINED)
}

fn window() -> JsValue {
    js_sys::global().into()
}

fn call(target: &JsValue, method: &str, args: &[&JsValue]) -> Result<JsValue, JsValue> {
    let function: Function = get(target, method).dyn_into()?;
    let array = Array::new();
    for arg in args {
        array.push(arg);
    }
    function.apply(target, &array)
}

async fn awaited(value: Result<JsValue, JsValue>) -> Result<JsValue, JsValue> {
    JsFuture::from(Promise::resolve(&value?)).await
}

/// What a wallet threw or rejected with, in a flow's terms: a person's "no"
/// is a rejection, whatever the wallet's wording.
pub fn error_of(value: &JsValue) -> SolError {
    let code = get(value, "code").as_f64().map(|code| code as i64);
    let message = get(value, "message")
        .as_string()
        .or_else(|| value.as_string())
        .unwrap_or_else(|| "the wallet refused it".to_string());
    let lower = message.to_ascii_lowercase();
    if code == Some(4001)
        || lower.contains("reject")
        || lower.contains("denied")
        || lower.contains("declined")
        || lower.contains("cancel")
    {
        return SolError::Rejected;
    }
    SolError::Wallet(clean(Some(message), 200))
}

fn clean(text: Option<String>, longest: usize) -> String {
    text.unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .take(longest)
        .collect()
}

fn safe_icon(icon: Option<String>) -> Option<String> {
    icon.filter(|icon| {
        icon.len() < 200_000
            && (icon.starts_with("data:image/svg+xml")
                || icon.starts_with("data:image/png")
                || icon.starts_with("data:image/webp")
                || icon.starts_with("data:image/jpeg")
                || icon.starts_with("data:image/gif"))
    })
}

fn bytes_of(value: &JsValue) -> Option<Vec<u8>> {
    value
        .dyn_ref::<Uint8Array>()
        .map(Uint8Array::to_vec)
        .or_else(|| {
            // Some wallets hand back a plain array of numbers.
            value.dyn_ref::<Array>().map(|array| {
                array
                    .iter()
                    .filter_map(|n| n.as_f64().map(|n| n as u8))
                    .collect()
            })
        })
}

/// One account a wallet offers.
#[derive(Clone)]
pub struct StdAccount {
    pub address: Pubkey,
    /// The chains it may act on (`solana:devnet`), as the wallet says.
    pub chains: Vec<String>,
    object: JsValue,
}

impl StdAccount {
    fn read(object: JsValue) -> Option<Self> {
        let address: Pubkey = get(&object, "address").as_string()?.parse().ok()?;
        // The key the wallet gives must be the address it names.
        if let Some(key) = bytes_of(&get(&object, "publicKey"))
            && key != address.to_bytes()
        {
            return None;
        }
        let chains = get(&object, "chains")
            .dyn_into::<Array>()
            .map(|chains| chains.iter().filter_map(|c| c.as_string()).collect())
            .unwrap_or_default();
        Some(Self {
            address,
            chains,
            object,
        })
    }
}

/// A Wallet Standard wallet.
#[derive(Clone)]
pub struct StandardWallet {
    pub name: String,
    pub icon: Option<String>,
    object: JsValue,
}

impl PartialEq for StandardWallet {
    fn eq(&self, other: &Self) -> bool {
        Object::is(&self.object, &other.object)
    }
}

impl StandardWallet {
    fn feature(&self, name: &str) -> JsValue {
        get(&get(&self.object, "features"), name)
    }

    fn has(&self, name: &str) -> bool {
        self.feature(name).is_object()
    }

    /// Whether it is a Solana wallet the app can use: it names a Solana
    /// chain, connects, and signs Solana transactions.
    pub fn is_usable(&self) -> bool {
        let solana = get(&self.object, "chains")
            .dyn_into::<Array>()
            .map(|chains| {
                chains
                    .iter()
                    .any(|c| c.as_string().is_some_and(|c| c.starts_with("solana:")))
            })
            .unwrap_or(false);
        solana
            && self.has("standard:connect")
            && (self.has("solana:signAndSendTransaction") || self.has("solana:signTransaction"))
    }

    /// The accounts it has given this site.
    pub fn accounts(&self) -> Vec<StdAccount> {
        get(&self.object, "accounts")
            .dyn_into::<Array>()
            .map(|accounts| accounts.iter().filter_map(StdAccount::read).collect())
            .unwrap_or_default()
    }

    /// Ask for its accounts: `silent` never shows a prompt, and answers only
    /// if the wallet already allows this site.
    pub async fn connect(&self, silent: bool) -> Result<Vec<StdAccount>, SolError> {
        let feature = self.feature("standard:connect");
        let input = Object::new();
        if silent {
            let _ = Reflect::set(&input, &"silent".into(), &JsValue::TRUE);
        }
        let answer = awaited(call(&feature, "connect", &[&input]))
            .await
            .map_err(|error| error_of(&error))?;
        let accounts = get(&answer, "accounts")
            .dyn_into::<Array>()
            .map(|accounts| accounts.iter().filter_map(StdAccount::read).collect())
            .unwrap_or_else(|_| self.accounts());
        Ok(accounts)
    }

    pub async fn disconnect(&self) {
        let feature = self.feature("standard:disconnect");
        if feature.is_object() {
            let _ = awaited(call(&feature, "disconnect", &[])).await;
        }
    }

    /// Follow its `change` event (accounts, chains). The listener lives as
    /// long as the page.
    pub fn on_change(&self, listener: impl FnMut(JsValue) + 'static) {
        let feature = self.feature("standard:events");
        if !feature.is_object() {
            return;
        }
        let closure = Closure::<dyn FnMut(JsValue)>::new(listener);
        let _ = call(
            &feature,
            "on",
            &[
                &JsValue::from_str("change"),
                closure.as_ref().unchecked_ref(),
            ],
        );
        closure.forget();
    }

    /// Have the wallet sign `wire` for `chain` with `account`: it sends it
    /// itself where it can (`signAndSendTransaction`), else it signs and hands
    /// it back for the app to send (`signTransaction`).
    pub async fn sign(
        &self,
        account: &StdAccount,
        wire: &[u8],
        chain: &str,
    ) -> Result<Signed, SolError> {
        let input = Object::new();
        let _ = Reflect::set(&input, &"account".into(), &account.object);
        let _ = Reflect::set(
            &input,
            &"transaction".into(),
            &Uint8Array::from(wire).into(),
        );
        let _ = Reflect::set(&input, &"chain".into(), &JsValue::from_str(chain));
        if self.has("solana:signAndSendTransaction") {
            let options = Object::new();
            let _ = Reflect::set(
                &options,
                &"preflightCommitment".into(),
                &JsValue::from_str("confirmed"),
            );
            let _ = Reflect::set(&input, &"options".into(), &options);
            let feature = self.feature("solana:signAndSendTransaction");
            let answer = awaited(call(&feature, "signAndSendTransaction", &[&input]))
                .await
                .map_err(|error| error_of(&error))?;
            let first = Reflect::get(&answer, &JsValue::from_f64(0.0)).unwrap_or(answer);
            let signature: [u8; 64] = bytes_of(&get(&first, "signature"))
                .and_then(|bytes| bytes.try_into().ok())
                .ok_or_else(|| SolError::Wallet("it answered without a signature".to_string()))?;
            return Ok(Signed::Sent(Sig(signature)));
        }
        let feature = self.feature("solana:signTransaction");
        let answer = awaited(call(&feature, "signTransaction", &[&input]))
            .await
            .map_err(|error| error_of(&error))?;
        let first = Reflect::get(&answer, &JsValue::from_f64(0.0)).unwrap_or(answer);
        let signed = bytes_of(&get(&first, "signedTransaction")).ok_or_else(|| {
            SolError::Wallet("it answered without a signed transaction".to_string())
        })?;
        Ok(Signed::Wire(signed))
    }
}

thread_local! {
    static REGISTERED: RefCell<Vec<StandardWallet>> = const { RefCell::new(Vec::new()) };
    static LISTENERS: RefCell<Vec<Rc<dyn Fn()>>> = const { RefCell::new(Vec::new()) };
    static API: RefCell<Option<JsValue>> = const { RefCell::new(None) };
}

fn register(object: JsValue) {
    if !object.is_object() {
        return;
    }
    let wallet = StandardWallet {
        name: clean(get(&object, "name").as_string(), 40),
        icon: safe_icon(get(&object, "icon").as_string()),
        object,
    };
    if wallet.name.is_empty() || !wallet.is_usable() {
        return;
    }
    let new = REGISTERED.with(|all| {
        let mut all = all.borrow_mut();
        if all.contains(&wallet) || all.iter().any(|w| w.name == wallet.name) {
            false
        } else {
            all.push(wallet);
            true
        }
    });
    if new {
        let listeners = LISTENERS.with(|l| l.borrow().clone());
        for listener in listeners {
            listener();
        }
    }
}

/// The object a wallet registers itself with.
fn api() -> JsValue {
    API.with(|slot| {
        if let Some(api) = slot.borrow().as_ref() {
            return api.clone();
        }
        let object = Object::new();
        let register = Closure::<dyn FnMut(JsValue) -> JsValue>::new(|wallet: JsValue| {
            register(wallet);
            // The standard hands back an unregister function; the app keeps
            // every wallet for the page's life.
            let noop = Closure::<dyn FnMut()>::new(|| {});
            let function: JsValue = noop.as_ref().clone();
            noop.forget();
            function
        });
        let _ = Reflect::set(
            &object,
            &"register".into(),
            register.as_ref().unchecked_ref(),
        );
        register.forget();
        let api: JsValue = object.into();
        *slot.borrow_mut() = Some(api.clone());
        api
    })
}

/// Find Solana wallets: listen for wallets that register later, and tell
/// the ones already loaded the app is ready. `on_new` runs whenever another
/// one registers.
pub fn discover(on_new: Rc<dyn Fn()>) {
    LISTENERS.with(|l| l.borrow_mut().push(on_new));
    let window = window();
    let first = API.with(|api| api.borrow().is_none());
    let api = api();
    if first {
        let api_for_events = api.clone();
        let listener = Closure::<dyn FnMut(JsValue)>::new(move |event: JsValue| {
            let callback = get(&event, "detail");
            if let Some(callback) = callback.dyn_ref::<Function>() {
                let _ = callback.call1(&JsValue::UNDEFINED, &api_for_events);
            }
        });
        let _ = call(
            &window,
            "addEventListener",
            &[
                &JsValue::from_str("wallet-standard:register-wallet"),
                listener.as_ref().unchecked_ref(),
            ],
        );
        listener.forget();
    }
    // `new CustomEvent("wallet-standard:app-ready", { detail: api })`.
    let constructor = get(&window, "CustomEvent");
    if let Some(constructor) = constructor.dyn_ref::<Function>() {
        let init = Object::new();
        let _ = Reflect::set(&init, &"detail".into(), &api);
        let args = Array::of2(&JsValue::from_str("wallet-standard:app-ready"), &init);
        if let Ok(event) = Reflect::construct(constructor, &args) {
            let _ = call(&window, "dispatchEvent", &[&event]);
        }
    }
}

/// Every usable Solana wallet registered so far.
pub fn registered() -> Vec<StandardWallet> {
    REGISTERED.with(|all| all.borrow().clone())
}

// Phantom's older provider --------------------------------------------------------------

/// `window.phantom.solana`, for a Phantom too old to register itself.
#[derive(Clone)]
pub struct Legacy(JsValue);

impl PartialEq for Legacy {
    fn eq(&self, other: &Self) -> bool {
        Object::is(&self.0, &other.0)
    }
}

impl Legacy {
    pub fn find() -> Option<Self> {
        let provider = get(&get(&window(), "phantom"), "solana");
        (provider.is_object() && get(&provider, "isPhantom").as_bool() == Some(true))
            .then_some(Self(provider))
    }

    fn key_of(value: &JsValue) -> Option<Pubkey> {
        let key = get(value, "publicKey");
        let key = if key.is_object() { key } else { value.clone() };
        call(&key, "toString", &[])
            .ok()
            .and_then(|text| text.as_string())
            .and_then(|text| text.parse().ok())
    }

    /// Ask for its account; `only_if_trusted` never shows a prompt.
    pub async fn connect(&self, only_if_trusted: bool) -> Result<Pubkey, SolError> {
        let options = Object::new();
        if only_if_trusted {
            let _ = Reflect::set(&options, &"onlyIfTrusted".into(), &JsValue::TRUE);
        }
        let answer = awaited(call(&self.0, "connect", &[&options]))
            .await
            .map_err(|error| error_of(&error))?;
        Self::key_of(&answer)
            .or_else(|| Self::key_of(&self.0))
            .ok_or_else(|| SolError::Wallet("it gave no account".to_string()))
    }

    pub async fn disconnect(&self) {
        let _ = awaited(call(&self.0, "disconnect", &[])).await;
    }

    /// Follow one of its events; the listener gets the new account, or none.
    pub fn on(&self, event: &str, mut listener: impl FnMut(Option<Pubkey>) + 'static) {
        let closure = Closure::<dyn FnMut(JsValue)>::new(move |value: JsValue| {
            listener(if value.is_null() || value.is_undefined() {
                None
            } else {
                Self::key_of(&value)
            })
        });
        let _ = call(
            &self.0,
            "on",
            &[&JsValue::from_str(event), closure.as_ref().unchecked_ref()],
        );
        closure.forget();
    }

    /// Sign and send, through the provider's request interface (the
    /// transaction in base58): the signature it sent.
    pub async fn sign_and_send(&self, wire: &[u8]) -> Result<Signed, SolError> {
        let params = Object::new();
        let _ = Reflect::set(
            &params,
            &"message".into(),
            &JsValue::from_str(&bs58::encode(wire).into_string()),
        );
        let request = Object::new();
        let _ = Reflect::set(
            &request,
            &"method".into(),
            &JsValue::from_str("signAndSendTransaction"),
        );
        let _ = Reflect::set(&request, &"params".into(), &params);
        let answer = awaited(call(&self.0, "request", &[&request]))
            .await
            .map_err(|error| error_of(&error))?;
        let signature = get(&answer, "signature")
            .as_string()
            .and_then(|text| Sig::parse(&text))
            .ok_or_else(|| SolError::Wallet("it answered without a signature".to_string()))?;
        Ok(Signed::Sent(signature))
    }
}
