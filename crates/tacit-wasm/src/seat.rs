//! A vault seat in the browser: key generation, and coordinating a FROST
//! signature over a vault payout.
//!
//! These wrap `tacit_vault::session`'s network-free state machines. The page
//! moves sealed envelopes between them and the coordinator; nothing it moves
//! can be read or forged by the coordinator.

use serde::{Deserialize, Serialize};
use tacit_core::envelope::SeatKey;
use tacit_light::chain::{Block, Tree};
use tacit_light::{build, keys, Net, Scanner};
use tacit_vault::frost::keys::{KeyPackage, PublicKeyPackage};
use tacit_vault::session::{Coordinator, DkgSession, Incoming, Outgoing, SeatPub, VaultShare};
use tacit_vault::Vault;
use wasm_bindgen::prelude::*;

use crate::app::{err, json, pk};

/// A fresh seat key: the secret, hex. Keep it with the wallet.
#[wasm_bindgen(js_name = newSeatKey)]
pub fn new_seat_key() -> String {
    hex::encode(SeatKey::generate().to_bytes())
}

#[wasm_bindgen(js_name = seatPublic)]
pub fn seat_public(secret: &str) -> Result<String, JsError> {
    Ok(hex::encode(seat_key(secret)?.public()))
}

fn seat_key(secret: &str) -> Result<SeatKey, JsError> {
    let b: [u8; 32] = hex::decode(secret).ok().and_then(|b| b.try_into().ok()).ok_or_else(|| err("bad seat key"))?;
    Ok(SeatKey::from_bytes(b))
}

fn seats(list: &str) -> Result<Vec<SeatPub>, JsError> {
    let list: Vec<String> = serde_json::from_str(list).map_err(err)?;
    list.iter()
        .map(|s| hex::decode(s).ok().and_then(|b| b.try_into().ok()).ok_or_else(|| err(format!("bad seat {s}"))))
        .collect()
}

#[derive(Serialize, Deserialize)]
struct Msg {
    #[serde(default)]
    seq: i64,
    #[serde(default)]
    from: String,
    to: String,
    round: u8,
    body: String,
}

fn out_json(out: &[Outgoing]) -> Vec<Msg> {
    out.iter()
        .map(|o| Msg { seq: 0, from: String::new(), to: hex::encode(o.to), round: o.round, body: hex::encode(&o.body) })
        .collect()
}

fn incoming(list: &str) -> Result<Vec<Incoming>, JsError> {
    let list: Vec<Msg> = serde_json::from_str(list).map_err(err)?;
    list.into_iter()
        .map(|m| {
            Ok(Incoming {
                from: hex::decode(&m.from).ok().and_then(|b| b.try_into().ok()).ok_or_else(|| err("bad sender"))?,
                round: m.round,
                body: hex::decode(&m.body).map_err(err)?,
            })
        })
        .collect()
}

/// A seat's secret share of a vault, as stored by the page.
#[derive(Serialize, Deserialize)]
pub struct ShareJson {
    pub seat: u16,
    pub seats: Vec<String>,
    pub key_package: String,
    pub public_key_package: String,
    pub fvk: String,
    pub ufvk: String,
    pub address: String,
}

fn share_json(net: Net, share: &VaultShare, seats: &[SeatPub]) -> Result<ShareJson, JsError> {
    Ok(ShareJson {
        seat: share.seat,
        seats: seats.iter().map(hex::encode).collect(),
        key_package: hex::encode(share.key_package.serialize().map_err(err)?),
        public_key_package: hex::encode(share.vault.public_key_package().serialize().map_err(err)?),
        fvk: hex::encode(share.vault.fvk().to_bytes()),
        ufvk: share.vault.ufvk().encode(&net),
        address: share.vault.encode_address(&net, 0),
    })
}

fn load_share(s: &str) -> Result<(ShareJson, KeyPackage, Vault), JsError> {
    let s: ShareJson = serde_json::from_str(s).map_err(err)?;
    let kp = KeyPackage::deserialize(&hex::decode(&s.key_package).map_err(err)?).map_err(err)?;
    let pkp = PublicKeyPackage::deserialize(&hex::decode(&s.public_key_package).map_err(err)?).map_err(err)?;
    let fvk: [u8; 96] = hex::decode(&s.fvk).ok().and_then(|b| b.try_into().ok()).ok_or_else(|| err("bad fvk"))?;
    let vault = Vault::from_parts(pkp, &fvk).map_err(err)?;
    Ok((s, kp, vault))
}

#[derive(Serialize)]
struct StepOut<T: Serialize> {
    out: Vec<Msg>,
    done: Option<T>,
}

/// This seat's side of a 2 of 3 key generation.
#[wasm_bindgen]
pub struct Dkg {
    net: Net,
    seats: Vec<SeatPub>,
    session: DkgSession,
    first: Vec<Outgoing>,
}

#[wasm_bindgen]
impl Dkg {
    #[wasm_bindgen(constructor)]
    pub fn new(network: &str, secret: &str, room: &str, seat_list: &str) -> Result<Dkg, JsError> {
        let net = Net::parse(network).ok_or_else(|| err("unknown network"))?;
        let seats = seats(seat_list)?;
        let (session, first) = DkgSession::start(seat_key(secret)?, room, seats.clone()).map_err(err)?;
        Ok(Dkg { net, seats, session, first })
    }

    /// Messages to send before anything has been received.
    pub fn start(&mut self) -> String {
        json(&out_json(&std::mem::take(&mut self.first)))
    }

    /// Feeds every message received so far (JSON from the coordinator's
    /// inbox). Returns `{out, done}`; `done` is the share once finished.
    pub fn step(&mut self, received: &str) -> Result<String, JsError> {
        let (out, done) = self.session.step(&incoming(received)?).map_err(err)?;
        let done = match done {
            Some(share) => Some(share_json(self.net, &share, &self.seats)?),
            None => None,
        };
        Ok(json(&StepOut { out: out_json(&out), done }))
    }
}

/// A vault seen through one seat's share: its balance, and payouts that this
/// seat proposes and coordinates.
#[wasm_bindgen]
pub struct VaultSeat {
    net: Net,
    share: ShareJson,
    key_package: KeyPackage,
    vault: Vault,
    scanner: Scanner,
    coordinator: Option<Coordinator>,
    first: Vec<Outgoing>,
}

#[wasm_bindgen]
impl VaultSeat {
    /// `birthday` is the tree state at the vault's creation height.
    #[wasm_bindgen(constructor)]
    pub fn new(network: &str, share: &str, birthday: &str) -> Result<VaultSeat, JsError> {
        let net = Net::parse(network).ok_or_else(|| err("unknown network"))?;
        let (share, key_package, vault) = load_share(share)?;
        let tree: Tree = serde_json::from_str(birthday).map_err(err)?;
        let scanner = Scanner::new(net, vault.fvk().clone(), &tree).map_err(err)?;
        Ok(VaultSeat { net, share, key_package, vault, scanner, coordinator: None, first: vec![] })
    }

    pub fn height(&self) -> u32 {
        self.scanner.height()
    }

    pub fn balance(&self) -> u64 {
        self.scanner.balance()
    }

    pub fn deposits(&self) -> u32 {
        self.scanner.found().iter().filter(|f| !f.change).count() as u32
    }

    pub fn spent(&self) -> Option<String> {
        self.scanner.spends().first().map(|s| s.0.clone())
    }

    pub fn scan(&mut self, blocks: &str) -> Result<(), JsError> {
        let blocks: Vec<Block> = serde_json::from_str(blocks).map_err(err)?;
        self.scanner.scan(&blocks).map_err(err)
    }

    /// The tree state a scan asked for ("need the tree state at block N").
    #[wasm_bindgen(js_name = setTree)]
    pub fn set_tree(&mut self, tree: &str) -> Result<(), JsError> {
        let tree: Tree = serde_json::from_str(tree).map_err(err)?;
        self.scanner.set_tree(&tree).map_err(err)
    }

    /// Builds and proves a payout of the whole vault to `to`, and starts
    /// coordinating its signature with `cosigner` (a seat number). Returns
    /// the amount paid.
    pub fn propose(&mut self, secret: &str, room: &str, to: &str, cosigner: u16) -> Result<u64, JsError> {
        let addr = keys::decode_address(self.net, to).map_err(err)?;
        let (pczt, amount) = build::vault_payout(&self.scanner, pk(), addr).map_err(err)?;
        let seats = seats(&serde_json::to_string(&self.share.seats).unwrap())?;
        let (c, first) = Coordinator::start(
            seat_key(secret)?,
            room,
            seats,
            self.vault.clone(),
            pczt,
            &[self.share.seat, cosigner],
            Some(self.key_package.clone()),
        )
        .map_err(err)?;
        self.coordinator = Some(c);
        self.first = first;
        Ok(amount)
    }

    pub fn start(&mut self) -> String {
        json(&out_json(&std::mem::take(&mut self.first)))
    }

    /// Feeds the signing room's messages. `done` is the finished transaction
    /// (`{txid, data}`) once both signatures are in.
    pub fn step(&mut self, received: &str) -> Result<String, JsError> {
        let c = self.coordinator.as_mut().ok_or_else(|| err("no payout in progress"))?;
        let (out, done) = c.step(&incoming(received)?).map_err(err)?;
        let done = match done {
            Some(pczt) => {
                let b = build::extract(pczt).map_err(err)?;
                Some(serde_json::json!({ "txid": b.txid, "data": hex::encode(b.data) }))
            }
            None => None,
        };
        Ok(json(&StepOut { out: out_json(&out), done }))
    }
}
