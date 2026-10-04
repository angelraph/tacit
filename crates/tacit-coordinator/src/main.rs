//! Runs the Tacit coordinator.
//!
//!   tacit-coordinator [listen-addr] [db-path]
//!
//! Defaults to 127.0.0.1:8650 and tacit-coordinator.sqlite.

use tacit_coordinator::{router, AppState};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let addr = args.get(1).map(String::as_str).unwrap_or("127.0.0.1:8650");
    let db = args.get(2).map(String::as_str).unwrap_or("tacit-coordinator.sqlite");
    let state = AppState::open(db).expect("open coordinator database");
    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind listen address");
    println!("tacit-coordinator listening on http://{addr} (db: {db})");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .expect("server error");
}
