//! Usage: tacit-regtest mine <count> [rpc-url]
//!        tacit-regtest height [rpc-url]

use tacit_regtest::{height, mine, DEFAULT_RPC};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let result = match args.get(1).map(String::as_str) {
        Some("mine") => {
            let count: u32 = args.get(2).and_then(|n| n.parse().ok()).unwrap_or(1);
            let rpc = args.get(3).map(String::as_str).unwrap_or(DEFAULT_RPC);
            mine(rpc, count).map(|h| format!("mined up to block {h}"))
        }
        Some("height") => {
            let rpc = args.get(2).map(String::as_str).unwrap_or(DEFAULT_RPC);
            height(rpc).map(|h| h.to_string())
        }
        _ => Err("usage: tacit-regtest mine <count> [rpc-url] | tacit-regtest height [rpc-url]".into()),
    };
    match result {
        Ok(out) => println!("{out}"),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
