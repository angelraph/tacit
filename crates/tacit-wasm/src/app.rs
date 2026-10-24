//! What the Tacit web app calls: a wallet, bids, and auditing auctions.
//!
//! Everything crosses the boundary as JSON strings, so the JavaScript side
//! stays a thin shell around fetch() and the UI. Keys are made and used only
//! in here; the page never sends one anywhere.

use std::sync::OnceLock;

use orchard::circuit::ProvingKey;
use serde::Serialize;
use tacit_core::auction::Terms;
use tacit_core::beacon::Beacon;
use tacit_light::audit::Audit;
use tacit_light::build::{self, Built};
use tacit_light::chain::{Block, RawTx, Tree};
use tacit_light::scan::Saved;
use tacit_light::{Account, Net, Scanner};
use wasm_bindgen::prelude::*;

pub(crate) fn err(e: impl std::fmt::Display) -> JsError {
    JsError::new(&e.to_string())
}

pub(crate) fn json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).expect("serialisable")
}

static PROVING_KEY: OnceLock<ProvingKey> = OnceLock::new();

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
}

/// Builds the Ironwood proving key if it isn't built yet. Takes several
/// seconds; the app calls it in the background right after loading.
#[wasm_bindgen(js_name = warmProver)]
pub fn warm_prover() {
    PROVING_KEY.get_or_init(build::proving_key);
}

pub(crate) fn pk() -> &'static ProvingKey {
    PROVING_KEY.get_or_init(build::proving_key)
}

fn net(name: &str) -> Result<Net, JsError> {
    Net::parse(name).ok_or_else(|| err(format!("unknown network {name:?}")))
}

/// A fresh 24 word recovery phrase.
#[wasm_bindgen(js_name = newPhrase)]
pub fn new_phrase() -> Result<String, JsError> {
    let mut entropy = [0u8; 32];
    getrandom::getrandom(&mut entropy).map_err(err)?;
    Ok(bip39::Mnemonic::from_entropy(&entropy).map_err(err)?.to_string())
}

/// Whether a recovery phrase is a valid BIP 39 English phrase.
#[wasm_bindgen(js_name = checkPhrase)]
pub fn check_phrase(phrase: &str) -> bool {
    bip39::Mnemonic::parse_in(bip39::Language::English, phrase.trim()).is_ok()
}

fn seed(phrase: &str) -> Result<[u8; 64], JsError> {
    let m = bip39::Mnemonic::parse_in(bip39::Language::English, phrase.trim()).map_err(err)?;
    Ok(m.to_seed(""))
}

#[derive(Serialize)]
struct BuiltJson {
    txid: String,
    data: String,
    fee: u64,
    spent: Vec<String>,
}

impl From<Built> for BuiltJson {
    fn from(b: Built) -> Self {
        BuiltJson { txid: b.txid, data: hex::encode(b.data), fee: b.fee, spent: b.spent.iter().map(hex::encode).collect() }
    }
}

#[derive(Serialize)]
struct NoteJson {
    txid: String,
    action: u32,
    height: u32,
    value: u64,
    change: bool,
    spent_in: Option<String>,
    pending: Option<String>,
}

/// A bidder's wallet.
#[wasm_bindgen]
pub struct Wallet {
    account: Account,
    scanner: Scanner,
}

#[wasm_bindgen]
impl Wallet {
    /// A new wallet that scans from right after `birthday` (tree state JSON
    /// from the chain gateway).
    #[wasm_bindgen(constructor)]
    pub fn new(network: &str, phrase: &str, birthday: &str) -> Result<Wallet, JsError> {
        let net = net(network)?;
        let account = Account::from_seed(net, &seed(phrase)?).map_err(err)?;
        let tree: Tree = serde_json::from_str(birthday).map_err(err)?;
        let scanner = Scanner::new(net, account.fvk().clone(), &tree).map_err(err)?;
        Ok(Wallet { account, scanner })
    }

    /// Restores a wallet saved with [`Wallet::save`].
    pub fn load(phrase: &str, saved: &str) -> Result<Wallet, JsError> {
        let saved: Saved = serde_json::from_str(saved).map_err(err)?;
        let account = Account::from_seed(saved.net, &seed(phrase)?).map_err(err)?;
        let scanner = Scanner::load(&saved).map_err(err)?;
        if scanner.fvk().to_bytes() != account.fvk().to_bytes() {
            return Err(err("that recovery phrase doesn't match the saved wallet"));
        }
        Ok(Wallet { account, scanner })
    }

    /// Everything but the keys, as JSON. Safe to keep in browser storage.
    pub fn save(&self) -> String {
        json(&self.scanner.save())
    }

    pub fn network(&self) -> String {
        self.account.net().name().into()
    }

    pub fn address(&self, index: u32) -> String {
        self.account.unified_address(index)
    }

    pub fn height(&self) -> u32 {
        self.scanner.height()
    }

    /// Spendable balance in zatoshi (not counting notes a pending
    /// transaction spends).
    pub fn spendable(&self) -> u64 {
        self.scanner.spendable()
    }

    pub fn balance(&self) -> u64 {
        self.scanner.balance()
    }

    pub fn notes(&self) -> String {
        json(
            &self
                .scanner
                .found()
                .iter()
                .map(|f| NoteJson {
                    txid: f.txid.clone(),
                    action: f.action,
                    height: f.height,
                    value: f.value(),
                    change: f.change,
                    spent_in: f.spent_in.clone(),
                    pending: f.pending.clone(),
                })
                .collect::<Vec<_>>(),
        )
    }

    /// Scans a batch of consecutive blocks (JSON from the chain gateway).
    pub fn scan(&mut self, blocks: &str) -> Result<(), JsError> {
        let blocks: Vec<Block> = serde_json::from_str(blocks).map_err(err)?;
        self.scanner.scan(&blocks).map_err(err)
    }

    /// Seals a bid and builds the bond deposit. Returns the transaction as
    /// JSON (`txid`, hex `data`, `fee`, `spent`). Call [`Wallet::sent`] once
    /// it has been broadcast.
    pub fn bid(&self, terms: &str, amount: u64, refund_index: u32) -> Result<String, JsError> {
        let terms: Terms = serde_json::from_str(terms).map_err(err)?;
        let built = build::bid(&self.account, &self.scanner, pk(), &terms, amount, refund_index).map_err(err)?;
        Ok(json(&BuiltJson::from(built)))
    }

    /// A plain shielded payment, with an optional text memo.
    pub fn send(&self, to: &str, value: u64, memo: &str) -> Result<String, JsError> {
        let memo = if memo.is_empty() {
            None
        } else {
            let bytes = memo.as_bytes();
            if bytes.len() > 512 {
                return Err(err("memo is longer than 512 bytes"));
            }
            let mut m = [0u8; 512];
            m[..bytes.len()].copy_from_slice(bytes);
            Some(m)
        };
        let built = build::send(&self.account, &self.scanner, pk(), to, value, memo).map_err(err)?;
        Ok(json(&BuiltJson::from(built)))
    }

    /// Marks the notes a broadcast transaction spends, so they aren't used
    /// twice before it is mined.
    pub fn sent(&mut self, built: &str) -> Result<(), JsError> {
        #[derive(serde::Deserialize)]
        struct B {
            txid: String,
            spent: Vec<String>,
        }
        let b: B = serde_json::from_str(built).map_err(err)?;
        let nfs: Vec<[u8; 32]> = b
            .spent
            .iter()
            .filter_map(|h| hex::decode(h).ok().and_then(|v| v.try_into().ok()))
            .collect();
        self.scanner.mark_pending(&nfs, &b.txid);
        Ok(())
    }

    /// Releases notes held by transactions that never got mined.
    #[wasm_bindgen(js_name = clearPending)]
    pub fn clear_pending(&mut self) {
        self.scanner.clear_pending();
    }
}

/// An audit of one auction, from its public terms.
#[wasm_bindgen(js_name = Audit)]
pub struct AuditJs {
    inner: Audit,
}

#[wasm_bindgen(js_class = Audit)]
impl AuditJs {
    /// `birthday` is the tree state at the terms' `created_height`.
    #[wasm_bindgen(constructor)]
    pub fn new(terms: &str, birthday: &str) -> Result<AuditJs, JsError> {
        let terms: Terms = serde_json::from_str(terms).map_err(err)?;
        let tree: Tree = serde_json::from_str(birthday).map_err(err)?;
        Ok(AuditJs { inner: Audit::new(terms, &tree).map_err(err)? })
    }

    pub fn height(&self) -> u32 {
        self.inner.height()
    }

    #[wasm_bindgen(js_name = vaultAddress)]
    pub fn vault_address(&self) -> String {
        self.inner.vault_address()
    }

    pub fn scan(&mut self, blocks: &str) -> Result<(), JsError> {
        let blocks: Vec<Block> = serde_json::from_str(blocks).map_err(err)?;
        self.inner.scanner().scan(&blocks).map_err(err)
    }

    /// JSON list of txids whose full transactions the audit still needs.
    pub fn wanted(&self) -> String {
        json(&self.inner.wanted())
    }

    #[wasm_bindgen(js_name = addTx)]
    pub fn add_tx(&mut self, txid: &str, raw: &str) -> Result<(), JsError> {
        let raw: RawTx = serde_json::from_str(raw).map_err(err)?;
        self.inner.add_tx(txid, &raw).map_err(err)
    }

    /// The audit report as JSON. Pass the closing round's beacon signature
    /// (hex) once it exists, or an empty string before that.
    pub fn report(&self, beacon_signature: &str) -> Result<String, JsError> {
        let beacon = if beacon_signature.is_empty() {
            None
        } else {
            let sig = hex::decode(beacon_signature).map_err(err)?;
            Some(Beacon::verify(self.inner.terms().close_round, &sig).map_err(err)?)
        };
        Ok(json(&self.inner.report(beacon.as_ref()).map_err(err)?))
    }
}

/// The auction id the terms commit to (hex).
#[wasm_bindgen(js_name = auctionId)]
pub fn auction_id(terms: &str) -> Result<String, JsError> {
    let terms: Terms = serde_json::from_str(terms).map_err(err)?;
    terms.validate().map_err(err)?;
    Ok(hex::encode(terms.id()))
}

/// The escrow id the terms commit to (hex).
#[wasm_bindgen(js_name = escrowId)]
pub fn escrow_id(terms: &str) -> Result<String, JsError> {
    let terms: tacit_core::escrow::EscrowTerms = serde_json::from_str(terms).map_err(err)?;
    terms.validate().map_err(err)?;
    Ok(hex::encode(terms.id()))
}

/// Unix time (seconds) at which a drand quicknet round is published.
#[wasm_bindgen(js_name = roundTime)]
pub fn round_time(round: u64) -> u64 {
    tacit_core::seal::round_time(round)
}

/// The quicknet round published at or just before a unix time.
#[wasm_bindgen(js_name = roundAt)]
pub fn round_at(unix: u64) -> u64 {
    tacit_core::seal::round_at(unix)
}

/// Checks a beacon signature against the quicknet group key.
#[wasm_bindgen(js_name = verifyBeacon)]
pub fn verify_beacon(round: u64, signature: &str) -> bool {
    hex::decode(signature).ok().is_some_and(|s| Beacon::verify(round, &s).is_ok())
}

/// The vault deposit address encoded in a unified viewing key.
#[wasm_bindgen(js_name = vaultAddress)]
pub fn vault_address(network: &str, ufvk: &str) -> Result<String, JsError> {
    let net = net(network)?;
    let fvk = tacit_light::keys::decode_fvk(net, ufvk).map_err(err)?;
    Ok(tacit_light::keys::encode_address(net, &tacit_light::keys::vault_address(&fvk)))
}
