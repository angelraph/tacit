//! A read-mostly window onto a Zcash node for browsers.
//!
//! Browsers can't speak gRPC to lightwalletd (or to Zebra's built-in
//! lightwalletd server), so the coordinator relays the handful of calls a
//! light wallet needs as plain JSON. It only relays public chain data and
//! already-encrypted transactions: a browser trial-decrypts blocks itself and
//! never sends the coordinator a key, an address or a note.
//!
//! Compact blocks are trimmed to their Ironwood actions, since Tacit never
//! touches any other pool.

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use tonic::transport::{Channel, Endpoint};
use zcash_client_backend::proto::service::compact_tx_streamer_client::CompactTxStreamerClient;
use zcash_client_backend::proto::service::{
    BlockId, BlockRange, ChainSpec, Empty, GetAddressUtxosArg, RawTransaction, TxFilter,
};

use crate::{bad, ApiError, AppState};

/// Most blocks one request may ask for.
pub const MAX_BLOCKS: u64 = 500;

#[derive(Clone)]
pub struct Chain {
    client: CompactTxStreamerClient<Channel>,
}

impl Chain {
    /// Points at a lightwalletd-compatible gRPC server. Connects lazily, so the
    /// coordinator starts even while the node is down.
    pub fn new(url: &str) -> Result<Self, String> {
        let mut endpoint = Endpoint::from_shared(url.to_string()).map_err(|e| e.to_string())?;
        if url.starts_with("https://") {
            endpoint = endpoint
                .tls_config(tonic::transport::ClientTlsConfig::new().with_webpki_roots())
                .map_err(|e| e.to_string())?;
        }
        let channel = endpoint.connect_lazy();
        Ok(Chain { client: CompactTxStreamerClient::new(channel).max_decoding_message_size(64 << 20) })
    }
}

fn chain(s: &AppState) -> Result<CompactTxStreamerClient<Channel>, ApiError> {
    s.chain
        .as_ref()
        .map(|c| c.client.clone())
        .ok_or_else(|| ApiError(axum::http::StatusCode::SERVICE_UNAVAILABLE, "this coordinator has no chain gateway".into()))
}

fn upstream(e: tonic::Status) -> ApiError {
    let code = match e.code() {
        tonic::Code::NotFound => axum::http::StatusCode::NOT_FOUND,
        tonic::Code::InvalidArgument | tonic::Code::OutOfRange => axum::http::StatusCode::BAD_REQUEST,
        _ => axum::http::StatusCode::BAD_GATEWAY,
    };
    ApiError(code, format!("node: {}", e.message()))
}

#[derive(Serialize)]
pub struct Info {
    pub chain: String,
    pub tip: u64,
    pub tip_hash: String,
    pub node: String,
}

pub async fn info(State(s): State<AppState>) -> Result<Json<Info>, ApiError> {
    let mut c = chain(&s)?;
    let lightd = c.get_lightd_info(Empty {}).await.map_err(upstream)?.into_inner();
    let tip = c.get_latest_block(ChainSpec::default()).await.map_err(upstream)?.into_inner();
    // Zebra's regtest calls itself "test"; only regtest activates Sapling at
    // height 1.
    let chain = match lightd.chain_name.as_str() {
        "main" | "mainnet" => "main",
        _ if lightd.sapling_activation_height <= 1 => "regtest",
        _ => "test",
    };
    Ok(Json(Info {
        chain: chain.into(),
        tip: tip.height,
        tip_hash: hex::encode(tip.hash.iter().rev().copied().collect::<Vec<_>>()),
        node: format!("{} {}", lightd.vendor, lightd.version),
    }))
}

#[derive(Serialize)]
pub struct Tree {
    pub height: u64,
    pub hash: String,
    pub time: u32,
    /// Hex of the Ironwood note commitment tree frontier as of this block,
    /// in the legacy `CommitmentTree` encoding lightwalletd uses.
    pub ironwood_tree: String,
}

pub async fn tree(State(s): State<AppState>, Path(height): Path<u64>) -> Result<Json<Tree>, ApiError> {
    let ts = chain(&s)?
        .get_tree_state(BlockId { height, hash: vec![] })
        .await
        .map_err(upstream)?
        .into_inner();
    Ok(Json(Tree { height: ts.height, hash: ts.hash, time: ts.time, ironwood_tree: ts.ironwood_tree }))
}

#[derive(Serialize)]
pub struct Action {
    pub nf: String,
    pub cmx: String,
    pub epk: String,
    /// The first 52 bytes of the note ciphertext.
    pub ct: String,
}

#[derive(Serialize)]
pub struct Tx {
    /// Position of the transaction in its block.
    pub index: u64,
    /// Transaction id in the usual display (byte-reversed) order.
    pub txid: String,
    pub actions: Vec<Action>,
}

#[derive(Serialize)]
pub struct Block {
    pub height: u64,
    pub hash: String,
    pub prev_hash: String,
    pub time: u32,
    /// Size of the Ironwood note commitment tree after this block.
    pub ironwood_tree_size: u32,
    /// Only transactions with Ironwood actions.
    pub txs: Vec<Tx>,
}

fn display_hex(bytes: &[u8]) -> String {
    hex::encode(bytes.iter().rev().copied().collect::<Vec<_>>())
}

#[derive(Deserialize)]
pub struct Range {
    from: u64,
    to: u64,
}

pub async fn blocks(State(s): State<AppState>, Query(r): Query<Range>) -> Result<Json<Vec<Block>>, ApiError> {
    if r.from == 0 || r.to < r.from {
        return Err(bad("need 1 <= from <= to"));
    }
    if r.to - r.from >= MAX_BLOCKS {
        return Err(bad(format!("at most {MAX_BLOCKS} blocks per request")));
    }
    let mut stream = chain(&s)?
        .get_block_range(BlockRange {
            start: Some(BlockId { height: r.from, hash: vec![] }),
            end: Some(BlockId { height: r.to, hash: vec![] }),
            pool_types: vec![],
        })
        .await
        .map_err(upstream)?
        .into_inner();
    let mut out = Vec::new();
    while let Some(b) = stream.message().await.map_err(upstream)? {
        let txs = b
            .vtx
            .into_iter()
            .filter(|t| !t.ironwood_actions.is_empty())
            .map(|t| Tx {
                index: t.index,
                txid: display_hex(&t.txid),
                actions: t
                    .ironwood_actions
                    .into_iter()
                    .map(|a| Action {
                        nf: hex::encode(a.nullifier),
                        cmx: hex::encode(a.cmx),
                        epk: hex::encode(a.ephemeral_key),
                        ct: hex::encode(a.ciphertext),
                    })
                    .collect(),
            })
            .collect();
        out.push(Block {
            height: b.height,
            hash: display_hex(&b.hash),
            prev_hash: display_hex(&b.prev_hash),
            time: b.time,
            ironwood_tree_size: b.chain_metadata.map_or(0, |m| m.ironwood_commitment_tree_size),
            txs,
        });
    }
    Ok(Json(out))
}

#[derive(Serialize)]
pub struct RawTx {
    /// Mined height; 0 while in the mempool.
    pub height: u64,
    pub data: String,
}

pub async fn get_tx(State(s): State<AppState>, Path(txid): Path<String>) -> Result<Json<RawTx>, ApiError> {
    let mut hash = hex::decode(&txid).map_err(|_| bad("txid is not hex"))?;
    if hash.len() != 32 {
        return Err(bad("txid must be 32 bytes"));
    }
    hash.reverse();
    let raw = chain(&s)?
        .get_transaction(TxFilter { block: None, index: 0, hash })
        .await
        .map_err(upstream)?
        .into_inner();
    if raw.height == u64::MAX {
        return Err(ApiError(axum::http::StatusCode::NOT_FOUND, "transaction fell out of the main chain".into()));
    }
    Ok(Json(RawTx { height: raw.height, data: hex::encode(raw.data) }))
}

#[derive(Deserialize)]
pub struct Broadcast {
    pub data: String,
}

#[derive(Serialize)]
pub struct Sent {
    pub ok: bool,
}

pub async fn send_tx(State(s): State<AppState>, Json(b): Json<Broadcast>) -> Result<Json<Sent>, ApiError> {
    let data = hex::decode(&b.data).map_err(|_| bad("data is not hex"))?;
    let res = chain(&s)?
        .send_transaction(RawTransaction { data, height: 0 })
        .await
        .map_err(upstream)?
        .into_inner();
    if res.error_code != 0 {
        return Err(bad(format!("node rejected the transaction: {} ({})", res.error_message, res.error_code)));
    }
    Ok(Json(Sent { ok: true }))
}

#[derive(Serialize)]
pub struct Utxo {
    pub txid: String,
    pub index: i32,
    pub script: String,
    pub value: i64,
    pub height: u64,
}

/// Unspent outputs of one transparent address. Only used to sweep a one-time
/// deposit address (cross-chain funding lands there) into Ironwood.
pub async fn utxos(State(s): State<AppState>, Path(addr): Path<String>) -> Result<Json<Vec<Utxo>>, ApiError> {
    let res = chain(&s)?
        .get_address_utxos(GetAddressUtxosArg { addresses: vec![addr], start_height: 0, max_entries: 100 })
        .await
        .map_err(upstream)?
        .into_inner();
    Ok(Json(
        res.address_utxos
            .into_iter()
            .map(|u| Utxo {
                txid: display_hex(&u.txid),
                index: u.index,
                script: hex::encode(u.script),
                value: u.value_zat,
                height: u.height,
            })
            .collect(),
    ))
}
