//! The executor as the keeper asks it, for the differential runs: each
//! vault's signed running total as the scenario scripts it by its chain's
//! time (its latest block's, read from the anvil it serves, so a pause or a
//! reorganisation that sets the chain's clock back moves its answers too),
//! and the attestation whose leaves and seal the keeper relays. Signatures
//! are not checked by the stand-in vaults, so none is made.

use std::sync::Arc;

use alloy::primitives::Address;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;

use super::node::Rpc;
use super::world::ExecutorScript;

/// A running executor stand-in; it stops with the test's runtime.
pub struct Executor {
    pub url: String,
}

struct Served {
    script: ExecutorScript,
    /// The chain whose time the answers follow.
    chain: Rpc,
}

impl Executor {
    pub async fn serve(script: ExecutorScript, chain: Rpc) -> Executor {
        let app = Router::new()
            .route("/usage/{chain}/{vault}", get(usage))
            .route("/attestation", get(attestation))
            .with_state(Arc::new(Served { script, chain }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a port for the executor");
        let url = format!("http://{}", listener.local_addr().expect("its address"));
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Executor { url }
    }
}

async fn usage(
    State(served): State<Arc<Served>>,
    Path((_chain, vault)): Path<(u64, String)>,
) -> Response {
    let Ok(vault) = vault.parse::<Address>() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok((_, now)) = served.chain.block_time(None).await else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let answer = served.script.usage.get(&vault).and_then(|steps| {
        steps
            .iter()
            .rev()
            .find(|(from, _)| *from <= now)
            .and_then(|(_, answer)| *answer)
    });
    match answer {
        Some((total, as_of)) => Json(json!({
            "total": total.to_string(),
            "as_of": as_of,
            "signature": "0x01",
        }))
        .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn attestation(State(served): State<Arc<Served>>) -> Response {
    Json(served.script.attestation.clone()).into_response()
}
