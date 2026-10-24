//! Runs the Tacit coordinator.
//!
//!   tacit-coordinator [listen-addr] [db-path] [lightwalletd-url]
//!
//! Defaults to 127.0.0.1:8650 and tacit-coordinator.sqlite. With a
//! lightwalletd URL (Zebra's gRPC port, say http://127.0.0.1:9067, or a
//! public server like https://testnet.zec.rocks:443) it also serves the chain
//! gateway browsers sync through.
//!
//! The same settings can come from the environment, which is how hosted
//! deployments configure it: PORT (listens on 0.0.0.0:$PORT), TACIT_DB,
//! TACIT_LIGHTWALLETD, and TACIT_WEB, a directory holding the web app to
//! serve next to the API.

use axum::http::{header::HeaderName, HeaderValue};
use tacit_coordinator::{router, AppState};
use tower_http::services::{ServeDir, ServeFile};
use tower_http::set_header::SetResponseHeaderLayer;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let addr = args
        .get(1)
        .cloned()
        .or_else(|| env("PORT").map(|p| format!("0.0.0.0:{p}")))
        .unwrap_or_else(|| "127.0.0.1:8650".into());
    let db = args.get(2).cloned().or_else(|| env("TACIT_DB")).unwrap_or_else(|| "tacit-coordinator.sqlite".into());
    let mut state = AppState::open(&db).expect("open coordinator database");
    if let Some(url) = args.get(3).cloned().or_else(|| env("TACIT_LIGHTWALLETD")) {
        state = state.with_chain(&url).expect("lightwalletd url");
        println!("relaying chain data from {url}");
    }
    let mut app = router(state);
    if let Some(dir) = env("TACIT_WEB") {
        // wasm-bindgen-rayon's worker helpers import a package by its folder,
        // which a bundler would resolve; answer those with the entry script.
        for pkg in ["pkg", "pkg-threads"] {
            let entry = ServeFile::new(format!("{dir}/{pkg}/tacit_wasm.js"));
            app = app.route_service(&format!("/{pkg}"), entry.clone()).route_service(&format!("/{pkg}/"), entry);
        }
        let header = |k: &'static str, v: &'static str| {
            SetResponseHeaderLayer::overriding(HeaderName::from_static(k), HeaderValue::from_static(v))
        };
        app = app
            .fallback_service(ServeDir::new(&dir))
            // Cross-origin isolation is what lets the prover use threads.
            .layer(header("cross-origin-opener-policy", "same-origin"))
            .layer(header("cross-origin-embedder-policy", "require-corp"));
        println!("serving the web app from {dir}");
    }
    let listener = tokio::net::TcpListener::bind(&addr).await.expect("bind listen address");
    println!("tacit-coordinator listening on http://{addr} (db: {db})");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .expect("server error");
}
