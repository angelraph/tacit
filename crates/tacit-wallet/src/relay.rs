//! Talking to a Tacit coordinator over HTTP, and driving the vault session
//! state machines with it.
//!
//! The coordinator only ever sees sealed envelopes; this module just moves
//! them and polls for new ones.

use std::time::{Duration, Instant};

use pczt::Pczt;
use serde_json::{json, Value};
use tacit_core::envelope::SeatKey;
use tacit_vault::frost::keys::KeyPackage;
use tacit_vault::session::{Coordinator, DkgSession, Incoming, Outgoing, SeatPub, SessionError, Signer, VaultShare};
use tacit_vault::Vault;

#[derive(Debug, thiserror::Error)]
pub enum RelayError {
    #[error("coordinator: {0}")]
    Http(String),
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error("timed out waiting for the other seats")]
    Timeout,
    #[error("signing was declined")]
    Declined,
}

pub struct Relay {
    base: String,
}

impl Relay {
    pub fn new(base: impl Into<String>) -> Self {
        Relay { base: base.into().trim_end_matches('/').to_string() }
    }

    fn post(&self, path: &str, body: &Value) -> Result<Value, RelayError> {
        let mut res = ureq::post(&format!("{}{path}", self.base))
            .header("content-type", "application/json")
            .send(body.to_string())
            .map_err(|e| RelayError::Http(e.to_string()))?;
        let text = res.body_mut().read_to_string().map_err(|e| RelayError::Http(e.to_string()))?;
        serde_json::from_str(&text).map_err(|e| RelayError::Http(e.to_string()))
    }

    fn get(&self, path: &str) -> Result<Value, RelayError> {
        let mut res = ureq::get(&format!("{}{path}", self.base)).call().map_err(|e| RelayError::Http(e.to_string()))?;
        let text = res.body_mut().read_to_string().map_err(|e| RelayError::Http(e.to_string()))?;
        serde_json::from_str(&text).map_err(|e| RelayError::Http(e.to_string()))
    }

    /// Opens a room for these seats and returns its id.
    pub fn open_room(&self, seats: &[SeatPub]) -> Result<String, RelayError> {
        let seats: Vec<String> = seats.iter().map(hex::encode).collect();
        let room = self.post("/v1/rooms", &json!({ "seats": seats }))?;
        room["id"].as_str().map(str::to_string).ok_or_else(|| RelayError::Http(format!("bad room reply: {room}")))
    }

    pub fn send(&self, room: &str, from: &SeatPub, out: &[Outgoing]) -> Result<(), RelayError> {
        for o in out {
            let msg = json!({
                "from": hex::encode(from),
                "to": hex::encode(o.to),
                "round": o.round,
                "body": hex::encode(&o.body),
            });
            let reply = self.post(&format!("/v1/rooms/{room}/messages"), &msg)?;
            if reply.get("error").is_some() {
                return Err(RelayError::Http(reply.to_string()));
            }
        }
        Ok(())
    }

    /// Messages addressed to `me` with sequence numbers above `after`.
    pub fn fetch(&self, room: &str, me: &SeatPub, after: i64) -> Result<Vec<(i64, Incoming)>, RelayError> {
        let list = self.get(&format!("/v1/rooms/{room}/messages?to={}&after={after}", hex::encode(me)))?;
        let list = list.as_array().ok_or_else(|| RelayError::Http(format!("bad inbox reply: {list}")))?;
        list.iter()
            .map(|m| {
                let from: [u8; 32] = hex::decode(m["from"].as_str().unwrap_or_default())
                    .ok()
                    .and_then(|b| b.try_into().ok())
                    .ok_or_else(|| RelayError::Http("bad sender".into()))?;
                let body = hex::decode(m["body"].as_str().unwrap_or_default()).map_err(|e| RelayError::Http(e.to_string()))?;
                Ok((m["seq"].as_i64().unwrap_or(0), Incoming { from, round: m["round"].as_u64().unwrap_or(0) as u8, body }))
            })
            .collect()
    }
}

/// Polls a room, keeping every message received so far.
struct Inbox<'a> {
    relay: &'a Relay,
    room: &'a str,
    me: SeatPub,
    after: i64,
    all: Vec<Incoming>,
}

impl<'a> Inbox<'a> {
    fn new(relay: &'a Relay, room: &'a str, me: SeatPub) -> Self {
        Inbox { relay, room, me, after: 0, all: Vec::new() }
    }

    fn refresh(&mut self) -> Result<(), RelayError> {
        for (seq, m) in self.relay.fetch(self.room, &self.me, self.after)? {
            self.after = self.after.max(seq);
            self.all.push(m);
        }
        Ok(())
    }
}

const POLL: Duration = Duration::from_millis(300);

/// Runs this seat's side of key generation to completion.
pub fn run_dkg(relay: &Relay, key: SeatKey, room: &str, seats: Vec<SeatPub>, timeout: Duration) -> Result<VaultShare, RelayError> {
    let me = key.public();
    let (mut session, out) = DkgSession::start(key, room, seats)?;
    relay.send(room, &me, &out)?;
    let mut inbox = Inbox::new(relay, room, me);
    let deadline = Instant::now() + timeout;
    loop {
        inbox.refresh()?;
        let (out, done) = session.step(&inbox.all)?;
        relay.send(room, &me, &out)?;
        if let Some(share) = done {
            return Ok(share);
        }
        if Instant::now() > deadline {
            return Err(RelayError::Timeout);
        }
        std::thread::sleep(POLL);
    }
}

/// Runs a signer: waits for the PCZT, asks `approve`, then contributes its
/// commitments and signature shares.
pub fn run_signer(
    relay: &Relay,
    key: SeatKey,
    room: &str,
    seats: Vec<SeatPub>,
    coordinator: SeatPub,
    key_package: KeyPackage,
    approve: impl FnOnce(&Pczt) -> bool,
    timeout: Duration,
) -> Result<(), RelayError> {
    let me = key.public();
    let mut signer = Signer::new(key, room, seats, coordinator, key_package)?;
    let mut inbox = Inbox::new(relay, room, me);
    let mut approve = Some(approve);
    let deadline = Instant::now() + timeout;
    loop {
        inbox.refresh()?;
        let mut declined = false;
        let out = signer.step(&inbox.all, |p| {
            let ok = approve.take().map(|f| f(p)).unwrap_or(false);
            declined = !ok;
            ok
        })?;
        relay.send(room, &me, &out)?;
        if signer.is_done() {
            return if declined { Err(RelayError::Declined) } else { Ok(()) };
        }
        if Instant::now() > deadline {
            return Err(RelayError::Timeout);
        }
        std::thread::sleep(POLL);
    }
}

/// Runs the coordinating seat: distributes the PCZT, gathers commitments and
/// shares from `signers` (seat numbers), and returns the authorized PCZT.
#[allow(clippy::too_many_arguments)]
pub fn run_coordinator(
    relay: &Relay,
    key: SeatKey,
    room: &str,
    seats: Vec<SeatPub>,
    vault: Vault,
    pczt: Pczt,
    signers: &[u16],
    own_key: Option<KeyPackage>,
    timeout: Duration,
) -> Result<Pczt, RelayError> {
    let me = key.public();
    let (mut coord, out) = Coordinator::start(key, room, seats, vault, pczt, signers, own_key)?;
    relay.send(room, &me, &out)?;
    let mut inbox = Inbox::new(relay, room, me);
    let deadline = Instant::now() + timeout;
    loop {
        inbox.refresh()?;
        let (out, done) = coord.step(&inbox.all)?;
        relay.send(room, &me, &out)?;
        if let Some(signed) = done {
            return Ok(signed);
        }
        if Instant::now() > deadline {
            return Err(RelayError::Timeout);
        }
        std::thread::sleep(POLL);
    }
}
