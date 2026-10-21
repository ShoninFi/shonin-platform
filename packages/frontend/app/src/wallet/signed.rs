//! The highest nonce the page signed at, per chain and account, and every
//! transaction it signed there.
//!
//! The record lives in the browser's storage and nowhere else, so a reload
//! or another tab of the same site sees it. Every read and every write goes
//! to storage at that moment: a page-signed send whose record storage can't
//! read or write sends nothing ([`Unavailable`]).
//!
//! Nothing else is kept: not what became of each send, nor any nonce below
//! the highest. Which nonce the next send signs at follows from this record
//! and the chain's mined count alone (see
//! [`super::session::sign_and_send`]).
//!
//! Sends for one chain and account are taken one at a time: within a tab
//! by a queue, and across tabs by the browser's Web Locks where it has
//! them.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use alloy_primitives::{Address, B256, Bytes, hex};
use futures::lock::{Mutex, OwnedMutexGuard};
use serde_json::{Value, json};

use super::phrase::Fees;

/// One transaction signed in the page: its hash, what it called, and the
/// fees it offered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedSend {
    pub hash: B256,
    pub to: Address,
    pub data: Bytes,
    pub fees: Fees,
}

/// The highest nonce the page signed at, with every transaction it signed
/// there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Highest {
    pub nonce: u64,
    pub sends: Vec<SignedSend>,
}

impl Highest {
    /// The hashes signed at the nonce.
    pub fn hashes(&self) -> Vec<B256> {
        self.sends.iter().map(|send| send.hash).collect()
    }
}

/// Storage that can't be read or written just now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unavailable;

/// Text kept by key, as the browser's local storage keeps it. Every access
/// can fail.
pub trait SendStorage {
    fn get(&self, key: &str) -> Result<Option<String>, Unavailable>;
    fn set(&self, key: &str, value: &str) -> Result<(), Unavailable>;
    fn remove(&self, key: &str) -> Result<(), Unavailable>;
}

/// Storage in memory, shared by its clones: storage outside a browser.
#[derive(Clone, Default)]
pub struct MemoryStorage(Rc<RefCell<BTreeMap<String, String>>>);

impl SendStorage for MemoryStorage {
    fn get(&self, key: &str) -> Result<Option<String>, Unavailable> {
        Ok(self.0.borrow().get(key).cloned())
    }
    fn set(&self, key: &str, value: &str) -> Result<(), Unavailable> {
        self.0
            .borrow_mut()
            .insert(key.to_string(), value.to_string());
        Ok(())
    }
    fn remove(&self, key: &str) -> Result<(), Unavailable> {
        self.0.borrow_mut().remove(key);
        Ok(())
    }
}

/// The storage key of the record for `chain_id` and `account` (its address
/// in lowercase hex).
pub fn storage_key(chain_id: u64, account: Address) -> String {
    format!("shonin.signed.{chain_id}.{}", hex::encode_prefixed(account))
}

fn fees_json(fees: Fees) -> Value {
    match fees {
        Fees::Eip1559 {
            max_fee,
            max_priority_fee,
        } => json!({
            "maxFee": max_fee.to_string(),
            "maxPriorityFee": max_priority_fee.to_string(),
        }),
        Fees::Legacy { gas_price } => json!({ "gasPrice": gas_price.to_string() }),
    }
}

fn fees_of(value: &Value) -> Option<Fees> {
    let number = |key: &str| value[key].as_str()?.parse::<u128>().ok();
    match number("gasPrice") {
        Some(gas_price) => Some(Fees::Legacy { gas_price }),
        None => Some(Fees::Eip1559 {
            max_fee: number("maxFee")?,
            max_priority_fee: number("maxPriorityFee")?,
        }),
    }
}

fn to_text(highest: &Highest) -> String {
    let sends: Vec<Value> = highest
        .sends
        .iter()
        .map(|send| {
            json!({
                "hash": hex::encode_prefixed(send.hash),
                "to": hex::encode_prefixed(send.to),
                "data": hex::encode_prefixed(&send.data),
                "fees": fees_json(send.fees),
            })
        })
        .collect();
    json!({ "nonce": highest.nonce, "sends": sends }).to_string()
}

/// The record kept as `text`; text that doesn't read as one is no record.
fn from_text(text: &str) -> Option<Highest> {
    let value = serde_json::from_str::<Value>(text).ok()?;
    let sends = value["sends"]
        .as_array()?
        .iter()
        .map(|send| {
            Some(SignedSend {
                hash: send["hash"].as_str()?.parse().ok()?,
                to: send["to"].as_str()?.parse().ok()?,
                data: send["data"].as_str()?.parse().ok()?,
                fees: fees_of(&send["fees"])?,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(Highest {
        nonce: value["nonce"].as_u64()?,
        sends,
    })
}

/// Each chain and account's turn: sends from one account take it in order.
type Turns = BTreeMap<(u64, Address), Arc<Mutex<()>>>;

/// The record of the page's own sends, per chain and account, in storage,
/// and the turns sends take.
pub struct SignedSends {
    storage: Box<dyn SendStorage>,
    turns: RefCell<Turns>,
}

impl Default for SignedSends {
    /// A record in a [`MemoryStorage`] of its own.
    fn default() -> Self {
        Self::new(MemoryStorage::default())
    }
}

/// One send's turn for a chain and account: no other send for them, in
/// this tab or (with Web Locks) another, signs until it is dropped.
pub struct Turn {
    _tab: OwnedMutexGuard<()>,
    _tabs: Option<cross_tab::Lock>,
}

impl SignedSends {
    pub fn new(storage: impl SendStorage + 'static) -> Self {
        Self {
            storage: Box::new(storage),
            turns: RefCell::default(),
        }
    }

    /// The record kept in the browser's local storage.
    pub fn page() -> Self {
        #[cfg(target_arch = "wasm32")]
        {
            Self::new(browser::LocalStorage)
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            Self::default()
        }
    }

    /// Wait for the turn to send on `chain_id` from `account`.
    pub async fn turn(&self, chain_id: u64, account: Address) -> Turn {
        let queue = self
            .turns
            .borrow_mut()
            .entry((chain_id, account))
            .or_default()
            .clone();
        let tab = queue.lock_owned().await;
        let tabs = cross_tab::take(&format!(
            "shonin.send.{chain_id}.{}",
            hex::encode_prefixed(account)
        ))
        .await;
        Turn {
            _tab: tab,
            _tabs: tabs,
        }
    }

    /// The record for `chain_id` and `account`, read from storage now.
    pub fn get(&self, chain_id: u64, account: Address) -> Result<Option<Highest>, Unavailable> {
        let text = self.storage.get(&storage_key(chain_id, account))?;
        Ok(text.as_deref().and_then(from_text))
    }

    /// Write the record for `chain_id` and `account`, or clear it (`None`).
    pub fn set(
        &self,
        chain_id: u64,
        account: Address,
        highest: Option<&Highest>,
    ) -> Result<(), Unavailable> {
        let key = storage_key(chain_id, account);
        match highest {
            Some(highest) => self.storage.set(&key, &to_text(highest)),
            None => self.storage.remove(&key),
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod browser {
    use super::{SendStorage, Unavailable};

    /// The browser's local storage.
    pub struct LocalStorage;

    fn storage() -> Result<web_sys::Storage, Unavailable> {
        web_sys::window()
            .ok_or(Unavailable)?
            .local_storage()
            .map_err(|_| Unavailable)?
            .ok_or(Unavailable)
    }

    impl SendStorage for LocalStorage {
        fn get(&self, key: &str) -> Result<Option<String>, Unavailable> {
            storage()?.get_item(key).map_err(|_| Unavailable)
        }
        fn set(&self, key: &str, value: &str) -> Result<(), Unavailable> {
            storage()?.set_item(key, value).map_err(|_| Unavailable)
        }
        fn remove(&self, key: &str) -> Result<(), Unavailable> {
            storage()?.remove_item(key).map_err(|_| Unavailable)
        }
    }
}

/// A lock shared by every tab of the site, by name (the Web Locks API,
/// reached by name since its bindings aren't stable yet).
#[cfg(target_arch = "wasm32")]
mod cross_tab {
    use std::cell::RefCell;
    use std::rc::Rc;

    use futures::channel::oneshot;
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::{JsCast, JsValue};

    /// Held until dropped.
    pub struct Lock {
        release: js_sys::Function,
    }

    impl Drop for Lock {
        fn drop(&mut self) {
            let _ = self.release.call0(&JsValue::UNDEFINED);
        }
    }

    /// Wait for the lock `name`; `None` at once where the browser has no
    /// Web Locks, or when it refuses the request.
    pub async fn take(name: &str) -> Option<Lock> {
        let navigator = web_sys::window()?.navigator();
        let locks = js_sys::Reflect::get(&navigator, &JsValue::from_str("locks")).ok()?;
        if !locks.is_object() {
            return None;
        }
        let request: js_sys::Function = js_sys::Reflect::get(&locks, &JsValue::from_str("request"))
            .ok()?
            .dyn_into()
            .ok()?;
        // The lock is held until `held` resolves, which dropping the lock does.
        let mut release = None;
        let held = js_sys::Promise::new(&mut |resolve, _reject| release = Some(resolve));
        let release = release?;
        let (granted, wait) = oneshot::channel::<()>();
        let granted = Rc::new(RefCell::new(Some(granted)));
        let on_grant = {
            let granted = granted.clone();
            Closure::once_into_js(move |_lock: JsValue| -> JsValue {
                if let Some(granted) = granted.borrow_mut().take() {
                    let _ = granted.send(());
                }
                held.into()
            })
        };
        let asked: js_sys::Promise = request
            .call2(&locks, &JsValue::from_str(name), &on_grant)
            .ok()?
            .dyn_into()
            .ok()?;
        // A refused request ends the wait, without the lock.
        wasm_bindgen_futures::spawn_local(async move {
            if wasm_bindgen_futures::JsFuture::from(asked).await.is_err() {
                granted.borrow_mut().take();
            }
        });
        wait.await.ok()?;
        Some(Lock { release })
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod cross_tab {
    /// No other tabs outside a browser.
    pub enum Lock {}

    pub async fn take(_name: &str) -> Option<Lock> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_record_is_kept_as_the_nonce_and_each_sends_hash_call_and_fees() {
        let account = Address::repeat_byte(0xAB);
        assert_eq!(
            storage_key(8453, account),
            format!("shonin.signed.8453.0x{}", "ab".repeat(20))
        );
        let highest = Highest {
            nonce: 5,
            sends: vec![
                SignedSend {
                    hash: B256::repeat_byte(1),
                    to: Address::repeat_byte(0xaa),
                    data: Bytes::from(vec![1, 2]),
                    fees: Fees::Eip1559 {
                        max_fee: u128::MAX,
                        max_priority_fee: 3,
                    },
                },
                SignedSend {
                    hash: B256::repeat_byte(2),
                    to: Address::repeat_byte(0xbb),
                    data: Bytes::new(),
                    fees: Fees::Legacy { gas_price: 7 },
                },
            ],
        };
        let text = to_text(&highest);
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["nonce"], 5);
        assert_eq!(value["sends"][0]["hash"], format!("0x{}", "01".repeat(32)));
        assert_eq!(value["sends"][0]["data"], "0x0102");
        assert_eq!(value["sends"][0]["fees"]["maxFee"], u128::MAX.to_string());
        assert_eq!(value["sends"][1]["fees"]["gasPrice"], "7");
        assert_eq!(from_text(&text), Some(highest.clone()));
        assert_eq!(from_text("not a record"), None);
        let signed = SignedSends::default();
        signed.set(8453, account, Some(&highest)).unwrap();
        assert_eq!(signed.get(8453, account), Ok(Some(highest)));
        assert_eq!(signed.get(1, account), Ok(None));
        signed.set(8453, account, None).unwrap();
        assert_eq!(signed.get(8453, account), Ok(None));
    }
}
