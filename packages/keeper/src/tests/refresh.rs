//! The daily refresh of a vault's restricted list is sent only when a
//! simulation shows it would add to the list.

use std::process::{Child, Command, Stdio};

use super::*;

/// A stand-in vault, compiled from:
///
/// ```solidity
/// contract M {
///     bytes32[] public list;
///     bool public grows;
///     function setGrows(bool g) external { grows = g; }
///     function refreshRestricted() external { if (grows) list.push(bytes32(uint256(list.length + 1))); }
///     function restricted() external view returns (bytes32[] memory) { return list; }
/// }
/// ```
const LISTING_VAULT: &str = "6080604052348015600f57600080fd5b506104938061001f6000396000f3fe608060405234801561001057600080fd5b50600436106100575760003560e01c80637072c6b11461005c57806380c9419e1461007a5780638dd36650146100aa578063b9235beb146100c8578063b9372e1c146100d2575b600080fd5b6100646100ee565b60405161007191906102b7565b60405180910390f35b610094600480360381019061008f9190610314565b610146565b6040516100a19190610350565b60405180910390f35b6100b261016a565b6040516100bf9190610386565b60405180910390f35b6100d061017d565b005b6100ec60048036038101906100e791906103cd565b6101d2565b005b6060600080548060200260200160405190810160405280929190818152602001828054801561013c57602002820191906000526020600020905b815481526020019060010190808311610128575b5050505050905090565b6000818154811061015657600080fd5b906000526020600020016000915090505481565b600160009054906101000a900460ff1681565b600160009054906101000a900460ff16156101d057600060016000805490506101a69190610429565b60001b90806001815401808255809150506001900390600052602060002001600090919091909150555b565b80600160006101000a81548160ff02191690831515021790555050565b600081519050919050565b600082825260208201905092915050565b6000819050602082019050919050565b6000819050919050565b61022e8161021b565b82525050565b60006102408383610225565b60208301905092915050565b6000602082019050919050565b6000610264826101ef565b61026e81856101fa565b93506102798361020b565b8060005b838110156102aa5781516102918882610234565b975061029c8361024c565b92505060018101905061027d565b5085935050505092915050565b600060208201905081810360008301526102d18184610259565b905092915050565b600080fd5b6000819050919050565b6102f1816102de565b81146102fc57600080fd5b50565b60008135905061030e816102e8565b92915050565b60006020828403121561032a576103296102d9565b5b6000610338848285016102ff565b91505092915050565b61034a8161021b565b82525050565b60006020820190506103656000830184610341565b92915050565b60008115159050919050565b6103808161036b565b82525050565b600060208201905061039b6000830184610377565b92915050565b6103aa8161036b565b81146103b557600080fd5b50565b6000813590506103c7816103a1565b92915050565b6000602082840312156103e3576103e26102d9565b5b60006103f1848285016103b8565b91505092915050565b7f4e487b7100000000000000000000000000000000000000000000000000000000600052601160045260246000fd5b6000610434826102de565b915061043f836102de565b9250828201905080821115610457576104566103fa565b5b9291505056fea26469706673582212205ad44b18ee92c20b04f841c2cbfcd71d86a2fcd07679a2cae4dd87b95f0909f364736f6c634300081d0033";

alloy::sol! {
    interface IListing {
        function setGrows(bool g) external;
    }
}

/// A local chain, stopped when dropped.
struct Anvil(Child, String);

impl Drop for Anvil {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn anvil() -> Anvil {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let child = Command::new("anvil")
        .args(["--port", &port.to_string(), "--silent"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("anvil runs");
    let url = format!("http://127.0.0.1:{port}");
    for _ in 0..100 {
        if reqwest::Client::new()
            .post(&url)
            .json(&serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "eth_chainId", "params": []}))
            .send()
            .await
            .is_ok()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Anvil(child, url)
}

#[tokio::test]
async fn the_restricted_list_s_refresh_is_sent_only_when_it_would_add_to_the_list() {
    let chain = anvil().await;
    let config = chain::ChainConfig {
        label: "local".to_string(),
        rpc: chain.1.clone(),
        ws: None,
        dollar: "USDC".to_string(),
        native_usd: None,
        confirmations: None,
        public_rpc: chain.1.clone(),
        logs_rpc: None,
        log_range: None,
    };
    let client: Observed = Client::connect(&config, chain::Signing::ReadOnly)
        .await
        .unwrap()
        .into();
    let from = {
        use alloy::providers::Provider;
        client.provider().get_accounts().await.unwrap()[0]
    };
    let code = alloy::hex::decode(LISTING_VAULT).unwrap();
    let vault = client.deploy(from, code).await.unwrap();
    assert!(
        !restricted_would_grow(&client, vault).await,
        "a refresh that adds nothing would be sent"
    );
    client
        .send(from, vault, IListing::setGrowsCall { g: true })
        .await
        .unwrap();
    assert!(
        restricted_would_grow(&client, vault).await,
        "a refresh that adds a code would not be sent"
    );
}
