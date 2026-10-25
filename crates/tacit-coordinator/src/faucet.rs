//! Relays a local `tacit faucet` to browsers at `/v1/faucet/*`, passing on
//! the client's IP so the faucet can limit drips per person.

use axum::body::Bytes;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::AppState;

fn relay(url: String, body: Option<Vec<u8>>, client_ip: String) -> Result<(u16, String), String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(std::time::Duration::from_secs(180)))
        .build()
        .into();
    let mut res = match body {
        Some(b) => agent
            .post(&url)
            .header("content-type", "application/json")
            .header("x-forwarded-for", &client_ip)
            .send(&b[..]),
        None => agent.get(&url).call(),
    }
    .map_err(|e| e.to_string())?;
    let status = res.status().as_u16();
    let text = res.body_mut().read_to_string().map_err(|e| e.to_string())?;
    Ok((status, text))
}

async fn forward(s: &AppState, path: &str, body: Option<Vec<u8>>, ip: String) -> Response {
    let Some(base) = s.faucet.clone() else {
        return (StatusCode::NOT_FOUND, axum::Json(serde_json::json!({ "error": "this coordinator runs no faucet" })))
            .into_response();
    };
    let url = format!("{}{path}", base.trim_end_matches('/'));
    match tokio::task::spawn_blocking(move || relay(url, body, ip)).await {
        Ok(Ok((status, text))) => (
            StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
            [("content-type", "application/json")],
            text,
        )
            .into_response(),
        _ => (StatusCode::BAD_GATEWAY, axum::Json(serde_json::json!({ "error": "the faucet isn't answering" })))
            .into_response(),
    }
}

fn client_ip(headers: &HeaderMap, peer: std::net::SocketAddr) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|v| v.trim().to_string())
        .unwrap_or_else(|| peer.ip().to_string())
}

pub async fn status(State(s): State<AppState>, ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>, headers: HeaderMap) -> Response {
    forward(&s, "/status", None, client_ip(&headers, peer)).await
}

pub async fn drip(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    forward(&s, "/drip", Some(body.to_vec()), client_ip(&headers, peer)).await
}
