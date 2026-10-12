use super::*;
use alloy::primitives::{Address, B256, U256};
use alloy::sol_types::SolEvent;
use axum::{Json, Router, http::StatusCode, routing::post};
use chain::bindings::strategies::IStrategyRegistry;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
struct Fixture {
    phase: u8,
    coherent: bool,
    reads: Vec<(bool, Value)>,
}
fn header(number: u64, hash: u8, parent: u8) -> Value {
    let mut b = alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
    b.header.inner.number = number;
    b.header.inner.timestamp = 100 + number;
    // Both backends share blocks zero through eight before their fork.
    b.header.inner.parent_hash = if number == 0 {
        B256::ZERO
    } else if number <= 8 || (number == 9 && hash == 109) {
        B256::repeat_byte((number - 1) as u8)
    } else {
        B256::repeat_byte(parent)
    };
    b.header.hash = if number <= 8 {
        B256::repeat_byte(number as u8)
    } else {
        B256::repeat_byte(hash)
    };
    if number == 9 && (hash == 9 || hash == 109) {
        b.header
            .inner
            .logs_bloom
            .accrue(alloy::primitives::BloomInput::Raw(
                Address::repeat_byte(3).as_slice(),
            ));
    }
    serde_json::to_value(b).unwrap()
}
fn registration(id: u64, hash: u8) -> Value {
    json!({"address":format!("{:?}",Address::repeat_byte(3)),"topics":[format!("{:?}",IStrategyRegistry::Registered::SIGNATURE_HASH),format!("0x{:064x}",U256::from(id)),format!("0x{:064x}",U256::from(1))],"data":format!("0x{}{}{}","11".repeat(32),"22".repeat(32),"33".repeat(32)),"blockNumber":"0x9","blockHash":format!("{:?}",B256::repeat_byte(hash)),"transactionHash":format!("{:?}",B256::repeat_byte(77)),"transactionIndex":"0x0","logIndex":"0x0","removed":false,"blockTimestamp":"0x6d"})
}
async fn exercise(
    coherent: bool,
    same_height: bool,
    contaminated: bool,
    new_event: bool,
    catch_up: bool,
) {
    let s = Arc::new(Mutex::new(Fixture {
        phase: 0,
        coherent,
        reads: vec![],
    }));
    let mut urls = vec![];
    let mut tasks = vec![];
    for old in [true, false] {
        let s = s.clone();
        let app=Router::new().route("/",post(move|Json(req):Json<Value>| {let s=s.clone();async move {
       let (phase,coherent)={let mut f=s.lock().unwrap();f.reads.push((old,req.clone()));(f.phase,f.coherent)};
       let height=if phase==0 {10}else if old {if contaminated{12}else{10}}else if same_height{10}else if catch_up {100+2*u64::from(phase-1)}else{12};
       let method=req["method"].as_str().unwrap();let result=match method {
         "eth_chainId"=>json!("0x7a69"),"eth_blockNumber"=>json!(format!("0x{height:x}")),
         "eth_getBlockByNumber"=>{
           let tag=req["params"][0].as_str().unwrap();
           if tag=="latest" {
             if old && phase>0 {return (StatusCode::SERVICE_UNAVAILABLE,Json(json!({"error":"latest temporarily unavailable"})));}
             if phase==0 {header(10,10,9)}else if same_height{header(10,110,109)}else if catch_up {header(height,100+height as u8,99+height as u8)}else{header(12,112,111)}
           }else{
             let n=u64::from_str_radix(tag.trim_start_matches("0x"),16).unwrap();
             if phase>0 && ((old && (n>10 || coherent)) || (!old && n<=10 && !coherent && !same_height)) {return (StatusCode::SERVICE_UNAVAILABLE,Json(json!({"error":"numbered route temporarily unavailable"})));}
             if phase>0 && (!old || coherent) {header(n,100+n as u8,99+n as u8)}else{header(n,n as u8,n.saturating_sub(1)as u8)}
           }
         },
         "eth_getBlockByHash"=>{
           let hash:B256=req["params"][0].as_str().unwrap().parse().unwrap();
           let byte=hash[0];
           let branch=if phase>0 && (!old || coherent) {100}else{0};
           if (byte<=8 || (byte>=branch && (byte-branch)as u64<=height)) && hash==B256::repeat_byte(byte) {
             header(if byte<=8{byte as u64}else{(byte-branch)as u64},byte,byte.saturating_sub(1))
           }else{Value::Null}
         },
         "eth_call"=>json!(format!("0x{:064x}{:064x}",32,0)),
         "eth_getLogs"=>{
           let f=&req["params"][0];
           let matches=f["topics"].get(0).is_none_or(|t| t==&json!(format!("{:?}",IStrategyRegistry::Registered::SIGNATURE_HASH)));
           if let Some(hash)=f["blockHash"].as_str() {
             let hash:B256=hash.parse().unwrap();let byte=hash[0];
             let branch=if phase>0 && (!old || coherent) {100}else{0};
             if (byte>8 && (byte<branch || (byte-branch)as u64>height)) || hash!=B256::repeat_byte(byte) {
               return (StatusCode::OK,Json(json!({"jsonrpc":"2.0","id":req["id"],"error":{"code":-32001,"message":"unknown block hash"}})));
             }
             if matches && ((!new_event && branch==0 && byte==9)||(new_event && branch==100 && byte==109)) {
               json!([registration(if branch==0{7}else{8},byte)])
             }else{json!([])}
           }else if !new_event && (phase==0 || (contaminated && old)) {
             let from=u64::from_str_radix(f["fromBlock"].as_str().unwrap().trim_start_matches("0x"),16).unwrap();
             let to=u64::from_str_radix(f["toBlock"].as_str().unwrap().trim_start_matches("0x"),16).unwrap();
             if from<=9 && to>=9 && matches {json!([registration(7,9)])}else{json!([])}
           }else{json!([])}
         },
         _=>panic!("unexpected RPC {}",req)
       };(StatusCode::OK,Json(json!({"jsonrpc":"2.0","id":req["id"],"result":result})))
     }}));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        urls.push(format!("http://{}", l.local_addr().unwrap()));
        tasks.push(tokio::spawn(async move {
            axum::serve(l, app).await.unwrap();
        }));
    }
    let url = urls.join(" ");
    let c = chain::Client::connect(
        &chain::ChainConfig {
            label: "owned fork fixture".into(),
            rpc: url.clone(),
            public_rpc: url.clone(),
            ws: None,
            dollar: "USDC".into(),
            native_usd: None,
            logs_rpc: None,
            log_range: None,
            confirmations: None,
        },
        chain::Signing::ReadOnly,
    )
    .await
    .unwrap();
    let a = Address::repeat_byte(3);
    let roles = chain::deployments::Roles {
        admin: a,
        platform: a,
        attestor: a,
        builder: a,
        reviewer: a,
        executor: a,
        keeper: a,
        operator: a,
        author: a,
        viewer: a,
    };
    let d = chain::Deployments {
        chain_key: "evm:31337".into(),
        chain_id: 31337,
        rpc_url: url.clone(),
        deployed_block: 1,
        usd: a,
        venue: a,
        attestation_verifier: a,
        attestation: a,
        publishing: a,
        prices: a,
        strategies: a,
        reviews: a,
        profiles: a,
        factory: a,
        station_factory: a,
        driver: a,
        bridge: a,
        closer: a,
        roles,
    };
    let indexer = Indexer::new(vec![Chain::Base], std::env::temp_dir(), None);
    indexer
        .add(Served {
            vault_chain: Chain::Base,
            key: "evm:31337".parse().unwrap(),
            client: c,
            deployments: d,
            native_usd: None,
            public_rpc: url,
        })
        .await;
    let reader = indexer.readers.read().unwrap()[0].clone();
    assert!(indexer.refresh_chain(&reader).await);
    {
        let p = indexer.parts.read().await;
        assert_eq!(p[&Chain::Base].info.block, 10);
        assert_eq!(p[&Chain::Base].feed.len(), usize::from(!new_event));
    }
    {
        let mut f = s.lock().unwrap();
        f.phase = 1;
        f.reads.clear();
    }
    let completed_at = *reader.read_at.lock().unwrap();
    indexer.refresh_chain(&reader).await;
    if catch_up {
        assert_eq!(indexer.parts.read().await[&Chain::Base].info.block, 10);
        assert_eq!(*reader.read_at.lock().unwrap(), completed_at);
        s.lock().unwrap().phase = 2;
        indexer.refresh_chain(&reader).await;
        let parts = indexer.parts.read().await;
        let read = &parts[&Chain::Base];
        assert_eq!(
            read.info.block, 100,
            "the fixed candidate must be the actual read scope"
        );
        assert_eq!(read.info.as_of, Some(read::moment(200)));
        assert!(!read.current, "an older candidate is still catching up");
        assert_eq!(
            *reader.read_at.lock().unwrap(),
            completed_at,
            "catch-up must not renew health freshness"
        );
        assert!(
            !indexer
                .health_at(
                    "indexer",
                    Instant::now()
                        + Duration::from_millis(reader.rpc_policy().indexer_stale_ms + 1)
                )
                .0
        );
        drop(parts);
        for _ in 0..4 {
            indexer.refresh_chain(&reader).await;
        }
        let parts = indexer.parts.read().await;
        assert_eq!(parts[&Chain::Base].info.block, 102);
        assert!(parts[&Chain::Base].current);
        assert!(*reader.read_at.lock().unwrap() > completed_at);
        drop(parts);
        for task in tasks {
            task.abort();
            let _ = task.await;
        }
        return;
    }
    let p = indexer.parts.read().await;
    let r = &p[&Chain::Base];
    let reads = s.lock().unwrap().reads.clone();
    println!(
        "coherent={coherent} same_height={same_height} published_head={} inherited_registration_count={} RPC={}",
        r.info.block,
        r.feed.len(),
        serde_json::to_string(&reads).unwrap()
    );
    assert_eq!(
        r.info.block,
        if same_height { 10 } else { 12 },
        "fixture must actually publish selected B head"
    );
    for t in tasks {
        t.abort();
        let _ = t.await;
    }
    assert_eq!(
        r.feed.len(),
        usize::from(new_event),
        "replaced or missing registration in selected branch feed"
    );
    if new_event {
        assert!(
            r.feed
                .iter()
                .any(|event| event.title.starts_with("Agent 8 "))
        );
    }
    assert!(
        reads
            .iter()
            .filter(|(_, req)| req["method"] == "eth_getLogs")
            .all(|(_, req)| req["params"][0].get("blockHash").is_some()),
        "record logs must be requested by exact hash"
    );
}
#[tokio::test]
async fn divergent_numbered_anchor_does_not_preserve_old_feed_as_current() {
    exercise(false, false, false, false, false).await;
}
#[tokio::test]
async fn coherent_numbered_history_rebuilds_feed() {
    exercise(true, false, false, false, false).await;
}
#[tokio::test]
async fn same_height_divergent_nodes_do_not_preserve_old_feed_as_current() {
    exercise(false, true, false, false, false).await;
}
#[tokio::test]
async fn rebuilt_history_logs_must_belong_to_selected_head() {
    exercise(false, false, true, false, false).await;
}
#[tokio::test]
async fn an_empty_old_fork_does_not_hide_a_selected_branch_registration() {
    exercise(false, false, true, true, false).await;
}

#[tokio::test]
async fn a_completed_older_candidate_keeps_its_date_and_does_not_renew_health() {
    exercise(true, false, false, false, true).await;
}
