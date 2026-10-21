//! Browser wallets: EIP-1193 providers, found by EIP-6963 announcements
//! (MetaMask, Rabby, Coinbase Wallet, Phantom's EVM side and every other
//! wallet that announces itself), with `window.ethereum` as the fallback.
//! This is the only place the app touches a wallet's JavaScript, through
//! `js_sys` alone.

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Array, Function, Object, Promise, Reflect};
use serde_json::Value;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

use super::rpc::{Fate, RpcError, Transport};

/// A wallet's provider object.
#[derive(Clone)]
pub struct Provider(JsValue);

impl PartialEq for Provider {
    fn eq(&self, other: &Self) -> bool {
        js_sys::Object::is(&self.0, &other.0)
    }
}

fn get(target: &JsValue, key: &str) -> JsValue {
    Reflect::get(target, &JsValue::from_str(key)).unwrap_or(JsValue::UNDEFINED)
}

fn to_js(value: &Value) -> JsValue {
    js_sys::JSON::parse(&value.to_string()).unwrap_or(JsValue::NULL)
}

fn from_js(value: &JsValue) -> Value {
    if value.is_undefined() {
        return Value::Null;
    }
    js_sys::JSON::stringify(value)
        .ok()
        .and_then(|text| text.as_string())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null)
}

/// An error a provider threw or rejected with.
fn error_of(value: &JsValue) -> RpcError {
    let json = from_js(value);
    if json.is_object() {
        let mut error = RpcError::from_json(&json);
        // An Error object's message isn't enumerable, so JSON leaves it out.
        if let Some(message) = get(value, "message").as_string() {
            error.message = message;
        }
        if error.code == 0
            && let Some(code) = get(value, "code").as_f64()
        {
            error.code = code as i64;
        }
        return error;
    }
    RpcError {
        code: get(value, "code")
            .as_f64()
            .map(|code| code as i64)
            .unwrap_or(-1),
        message: get(value, "message")
            .as_string()
            .unwrap_or_else(|| "the wallet refused it".to_string()),
        data: None,
        fate: Fate::Answered,
    }
}

impl Provider {
    pub fn is_usable(&self) -> bool {
        get(&self.0, "request").is_function()
    }

    /// Listen to one of the provider's events. The listener lives as long
    /// as the page.
    pub fn on(&self, event: &str, listener: impl FnMut(JsValue) + 'static) {
        let on = get(&self.0, "on");
        let Some(on) = on.dyn_ref::<Function>() else {
            return;
        };
        let closure = Closure::<dyn FnMut(JsValue)>::new(listener);
        let _ = on.call2(
            &self.0,
            &JsValue::from_str(event),
            closure.as_ref().unchecked_ref(),
        );
        closure.forget();
    }

    /// Flags a legacy provider sets, to name it.
    fn flag(&self, name: &str) -> bool {
        get(&self.0, name).as_bool() == Some(true)
    }
}

impl Transport for Provider {
    async fn request(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let request = get(&self.0, "request");
        let request: Function = request
            .dyn_into()
            .map_err(|_| RpcError::unreachable("that wallet doesn't take requests"))?;
        let arguments = Object::new();
        let _ = Reflect::set(&arguments, &"method".into(), &JsValue::from_str(method));
        let _ = Reflect::set(&arguments, &"params".into(), &to_js(&params));
        let promise = request
            .call1(&self.0, &arguments)
            .map_err(|error| error_of(&error))?;
        let answer = JsFuture::from(Promise::resolve(&promise))
            .await
            .map_err(|error| error_of(&error))?;
        Ok(from_js(&answer))
    }
}

/// A wallet that announced itself (EIP-6963), or the legacy injected one.
#[derive(Clone)]
pub struct Announced {
    /// Reverse-DNS id: `io.metamask`, `io.rabby`, `com.coinbase.wallet`,
    /// `app.phantom`. Empty for the legacy provider.
    pub rdns: String,
    pub name: String,
    /// A `data:image/…` URI, or none: anything else isn't drawn.
    pub icon: Option<String>,
    pub provider: Provider,
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

fn clean(text: Option<String>, longest: usize) -> String {
    text.unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .take(longest)
        .collect()
}

thread_local! {
    static FOUND: RefCell<Vec<Announced>> = const { RefCell::new(Vec::new()) };
    static LISTENING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Start listening for announcements and ask every wallet to announce
/// itself. Wallets answer asynchronously; `on_found` runs for each new one.
pub fn discover(on_found: Rc<dyn Fn()>) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let window: JsValue = window.into();
    if !LISTENING.with(|listening| listening.replace(true)) {
        let add = get(&window, "addEventListener");
        if let Some(add) = add.dyn_ref::<Function>() {
            let closure = Closure::<dyn FnMut(JsValue)>::new(move |event: JsValue| {
                let detail = get(&event, "detail");
                let info = get(&detail, "info");
                let provider = Provider(get(&detail, "provider"));
                if !provider.is_usable() {
                    return;
                }
                let announced = Announced {
                    rdns: clean(get(&info, "rdns").as_string(), 128),
                    name: clean(get(&info, "name").as_string(), 64),
                    icon: safe_icon(get(&info, "icon").as_string()),
                    provider,
                };
                if announced.rdns.is_empty() || announced.name.is_empty() {
                    return;
                }
                let added = FOUND.with(|found| {
                    let mut found = found.borrow_mut();
                    if found.iter().any(|known| known.rdns == announced.rdns) {
                        false
                    } else {
                        found.push(announced);
                        true
                    }
                });
                if added {
                    on_found();
                }
            });
            let _ = add.call2(
                &window,
                &JsValue::from_str("eip6963:announceProvider"),
                closure.as_ref().unchecked_ref(),
            );
            closure.forget();
        }
    }
    // Ask; wallets that loaded first answer this, later ones announce on
    // their own.
    let event = get(&window, "Event");
    if let Some(event) = event.dyn_ref::<Function>()
        && let Ok(event) = Reflect::construct(
            event,
            &Array::of1(&JsValue::from_str("eip6963:requestProvider")),
        )
    {
        let dispatch = get(&window, "dispatchEvent");
        if let Some(dispatch) = dispatch.dyn_ref::<Function>() {
            let _ = dispatch.call1(&window, &event);
        }
    }
}

/// Every wallet announced so far.
pub fn announced() -> Vec<Announced> {
    FOUND.with(|found| found.borrow().clone())
}

/// `window.ethereum`, for a wallet that predates announcements, named by
/// the flags it sets.
pub fn legacy() -> Option<Announced> {
    let window: JsValue = web_sys::window()?.into();
    let provider = Provider(get(&window, "ethereum"));
    if !provider.is_usable() {
        return None;
    }
    let name = if provider.flag("isRabby") {
        "Rabby"
    } else if provider.flag("isCoinbaseWallet") {
        "Coinbase Wallet"
    } else if provider.flag("isPhantom") {
        "Phantom"
    } else if provider.flag("isBraveWallet") {
        "Brave Wallet"
    } else if provider.flag("isMetaMask") {
        "MetaMask"
    } else {
        "Browser wallet"
    };
    Some(Announced {
        rdns: String::new(),
        name: name.to_string(),
        icon: None,
        provider,
    })
}
