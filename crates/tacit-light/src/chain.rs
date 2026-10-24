//! The JSON a coordinator's chain gateway serves (`/v1/chain/...`).
//!
//! Only data types live here; fetching is the caller's job (`fetch` in the
//! browser, any HTTP client natively), which keeps this crate free of I/O.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Info {
    pub chain: String,
    pub tip: u64,
    pub tip_hash: String,
    pub node: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tree {
    pub height: u64,
    pub hash: String,
    pub time: u32,
    pub ironwood_tree: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Action {
    pub nf: String,
    pub cmx: String,
    pub epk: String,
    pub ct: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tx {
    pub index: u64,
    /// Display (byte-reversed) order.
    pub txid: String,
    pub actions: Vec<Action>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Block {
    pub height: u64,
    pub hash: String,
    pub prev_hash: String,
    pub time: u32,
    pub ironwood_tree_size: u32,
    pub txs: Vec<Tx>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RawTx {
    pub height: u64,
    pub data: String,
}

/// Converts a display-order txid to the internal byte order.
pub fn txid_bytes(display: &str) -> crate::Result<[u8; 32]> {
    let mut b = crate::hex32(display)?;
    b.reverse();
    Ok(b)
}
