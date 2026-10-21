//! `RpcReader::call_at` asks `eth_call` with the standard params shape and
//! the block as a hex quantity.

use std::cell::RefCell;

use alloy_primitives::{Address, Bytes};
use app::tx::port::Reader;
use app::wallet::rpc::{RpcError, RpcReader, Transport};
use futures::executor::block_on;
use serde_json::{Value, json};

struct Records(RefCell<Vec<(String, Value)>>);

impl Transport for Records {
    async fn request(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        self.0.borrow_mut().push((method.to_string(), params));
        Ok(json!(
            "0x00000000000000000000000000000000000000000000000000000000000003e8"
        ))
    }
}

#[test]
fn an_eth_call_at_a_block_is_asked_with_the_block_in_hex() {
    let reader = RpcReader {
        transport: Records(RefCell::new(Vec::new())),
    };
    let to = Address::repeat_byte(0xdd);
    let answer =
        block_on(reader.call_at(to, Bytes::from(vec![0xdd, 0x62, 0xed, 0x3e]), 101)).unwrap();
    assert_eq!(answer.len(), 32);
    let asked = reader.transport.0.borrow();
    assert_eq!(asked[0].0, "eth_call");
    assert_eq!(
        asked[0].1,
        json!([{"to": format!("{to:?}"), "data": "0xdd62ed3e"}, "0x65"])
    );
    // Block zero is "0x0", never "0x".
    drop(asked);
    block_on(reader.call_at(to, Bytes::from(vec![0xdd]), 0)).unwrap();
    assert_eq!(reader.transport.0.borrow()[1].1[1], json!("0x0"));
}
