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
//!
//! Public lightwalletd servers hand out blocks at a few dozen a second, which
//! makes restoring an older wallet slow. So the gateway can keep a block
//! cache: every block buried deeper than [`CACHE_DEPTH`] is stored the first
//! time anyone asks for it, and a background warmer fills the cache from the
//! Ironwood activation height on. A cached range is served without touching
//! the node at all.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Response};
use axum::Json;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tonic::transport::{Channel, Endpoint};
use zcash_client_backend::proto::service::compact_tx_streamer_client::CompactTxStreamerClient;
use zcash_client_backend::proto::service::{
    BlockId, BlockRange, ChainSpec, Empty, GetAddressUtxosArg, RawTransaction, TxFilter,
};

use crate::{bad, ApiError, AppState};

/// Most blocks one request may ask for.
pub const MAX_BLOCKS: u64 = 500;

/// Blocks at least this deep are final enough to cache.
pub const CACHE_DEPTH: u64 = 100;

/// Where Ironwood begins; nothing Tacit can spend lies before it.
pub fn ironwood_activation(chain: &str) -> u64 {
    match chain {
        "main" => 3_428_143,
        "test" => 4_134_000,
        _ => 1,
    }
}

#[derive(Clone)]
pub struct Chain {
    client: CompactTxStreamerClient<Channel>,
    cache: Option<Arc<Mutex<Connection>>>,
    /// Latest tip seen, for deciding what is deep enough to cache.
    tip: Arc<AtomicU64>,
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
        Ok(Chain {
            client: CompactTxStreamerClient::new(channel).max_decoding_message_size(64 << 20),
            cache: None,
            tip: Arc::new(AtomicU64::new(0)),
        })
    }

    /// Keeps a block cache in the SQLite file at `path`.
    pub fn with_cache(mut self, path: &str) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS blocks (height INTEGER PRIMARY KEY, body TEXT NOT NULL);",
        )?;
        self.cache = Some(Arc::new(Mutex::new(conn)));
        Ok(self)
    }

    /// Fills the cache in the background, from `from` (or the Ironwood
    /// activation height) up to the tip, then keeps up as blocks arrive.
    pub fn spawn_warmer(&self, from: Option<u64>) {
        let Some(_) = &self.cache else { return };
        let me = self.clone();
        tokio::spawn(async move {
            let mut c = me.client.clone();
            let start = loop {
                match c.get_lightd_info(Empty {}).await {
                    Ok(i) => {
                        let name = if matches!(i.get_ref().chain_name.as_str(), "main" | "mainnet") { "main" } else { "test" };
                        break from.unwrap_or_else(|| ironwood_activation(name));
                    }
                    Err(e) => {
                        eprintln!("block cache: node unreachable ({}), retrying", e.message());
                        tokio::time::sleep(Duration::from_secs(30)).await;
                    }
                }
            };
            let mut h = start;
            println!("block cache: warming from {start}");
            loop {
                if let Ok(t) = c.get_latest_block(ChainSpec::default()).await {
                    me.tip.fetch_max(t.into_inner().height, Ordering::Relaxed);
                }
                let safe = me.tip.load(Ordering::Relaxed).saturating_sub(CACHE_DEPTH);
                while h <= safe {
                    let to = (h + MAX_BLOCKS - 1).min(safe);
                    if me.cached(h, to).is_none() {
                        match fetch(&mut c, h, to).await {
                            Ok(blocks) => me.store(&blocks),
                            Err(e) => {
                                eprintln!("block cache: {} at {h}, retrying", e.message());
                                tokio::time::sleep(Duration::from_secs(10)).await;
                                continue;
                            }
                        }
                    }
                    h = to + 1;
                }
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        });
    }

    /// Current tip height.
    pub async fn tip(&self) -> Result<u64, String> {
        let t = self.client.clone().get_latest_block(ChainSpec::default()).await.map_err(|e| e.message().to_string())?;
        let h = t.into_inner().height;
        self.tip.fetch_max(h, Ordering::Relaxed);
        Ok(h)
    }

    pub async fn tree_state(&self, height: u64) -> Result<Tree, String> {
        let ts = self
            .client
            .clone()
            .get_tree_state(BlockId { height, hash: vec![] })
            .await
            .map_err(|e| e.message().to_string())?
            .into_inner();
        Ok(Tree { height: ts.height, hash: ts.hash, time: ts.time, ironwood_tree: ts.ironwood_tree })
    }

    /// A block range as the gateway's JSON, from the cache when it can.
    pub async fn blocks_json(&self, from: u64, to: u64) -> Result<String, String> {
        if let Some(body) = self.cached(from, to) {
            return Ok(body);
        }
        let blocks = fetch(&mut self.client.clone(), from, to).await.map_err(|e| e.message().to_string())?;
        self.store(&blocks);
        serde_json::to_string(&blocks).map_err(|e| e.to_string())
    }

    pub async fn raw_tx(&self, txid: &str) -> Result<RawTx, String> {
        let mut hash = hex::decode(txid).map_err(|_| "txid is not hex".to_string())?;
        hash.reverse();
        let raw = self
            .client
            .clone()
            .get_transaction(TxFilter { block: None, index: 0, hash })
            .await
            .map_err(|e| e.message().to_string())?
            .into_inner();
        Ok(RawTx { height: raw.height, data: hex::encode(raw.data) })
    }

    /// The range as a JSON array, if every block in it is cached.
    fn cached(&self, from: u64, to: u64) -> Option<String> {
        let db = self.cache.as_ref()?.lock().unwrap();
        let mut stmt = db.prepare_cached("SELECT body FROM blocks WHERE height BETWEEN ?1 AND ?2 ORDER BY height").ok()?;
        let bodies: Vec<String> = stmt
            .query_map(params![from as i64, to as i64], |r| r.get(0))
            .ok()?
            .collect::<Result<_, _>>()
            .ok()?;
        (bodies.len() as u64 == to - from + 1).then(|| format!("[{}]", bodies.join(",")))
    }

    fn store(&self, blocks: &[Block]) {
        let Some(cache) = &self.cache else { return };
        let safe = self.tip.load(Ordering::Relaxed).saturating_sub(CACHE_DEPTH);
        let mut db = cache.lock().unwrap();
        let Ok(tx) = db.transaction() else { return };
        for b in blocks.iter().filter(|b| b.height <= safe) {
            if let Ok(body) = serde_json::to_string(b) {
                let _ = tx.execute("INSERT OR REPLACE INTO blocks (height, body) VALUES (?1, ?2)", params![b.height as i64, body]);
            }
        }
        let _ = tx.commit();
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
    if let Some(ch) = &s.chain {
        ch.tip.fetch_max(tip.height, Ordering::Relaxed);
    }
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

pub async fn blocks(State(s): State<AppState>, Query(r): Query<Range>) -> Result<Response, ApiError> {
    if r.from == 0 || r.to < r.from {
        return Err(bad("need 1 <= from <= to"));
    }
    if r.to - r.from >= MAX_BLOCKS {
        return Err(bad(format!("at most {MAX_BLOCKS} blocks per request")));
    }
    let ch = s.chain.as_ref().ok_or_else(|| {
        ApiError(axum::http::StatusCode::SERVICE_UNAVAILABLE, "this coordinator has no chain gateway".into())
    })?;
    if let Some(body) = ch.cached(r.from, r.to) {
        return Ok(([(CONTENT_TYPE, "application/json")], body).into_response());
    }
    let out = fetch(&mut chain(&s)?, r.from, r.to).await.map_err(upstream)?;
    ch.store(&out);
    Ok(Json(out).into_response())
}

async fn fetch(c: &mut CompactTxStreamerClient<Channel>, from: u64, to: u64) -> Result<Vec<Block>, tonic::Status> {
    let mut stream = c
        .get_block_range(BlockRange {
            start: Some(BlockId { height: from, hash: vec![] }),
            end: Some(BlockId { height: to, hash: vec![] }),
            pool_types: vec![],
        })
        .await?
        .into_inner();
    let mut out = Vec::new();
    while let Some(b) = stream.message().await? {
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
    Ok(out)
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
