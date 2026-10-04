//! Key generation and signing as message-passing state machines.
//!
//! Nothing here touches the network. Each step takes the messages a seat has
//! received and returns the messages it must send, already sealed for their
//! recipients with `tacit_core::envelope`. The same code drives the CLI over
//! HTTP and the browser over fetch.
//!
//! Every envelope is bound to `tacit/<room>/<round>`, so messages can't be
//! replayed into another room or round.

use std::collections::BTreeMap;

use blake2b_simd::Params;
use pczt::Pczt;
use rand_core::OsRng;
use tacit_core::envelope::{EnvelopeError, SeatKey};

use crate::dkg::{self, seat};
use crate::frost::keys::dkg::{round1, round2};
use crate::frost::keys::{KeyPackage, PublicKeyPackage};
use crate::frost::round1::SigningCommitments;
use crate::frost::round2::SignatureShare;
use crate::frost::Identifier;
use crate::{Round1, SigningRequest, Vault, VaultError};

pub type SeatPub = [u8; 32];

/// A sealed message ready to hand to the coordinator.
#[derive(Clone, Debug)]
pub struct Outgoing {
    pub to: SeatPub,
    pub round: u8,
    pub body: Vec<u8>,
}

/// A sealed message as received from the coordinator.
#[derive(Clone, Debug)]
pub struct Incoming {
    pub from: SeatPub,
    pub round: u8,
    pub body: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("message from {0} doesn't open: {1}")]
    Envelope(String, EnvelopeError),
    #[error("message from someone who isn't a seat in this room")]
    Stranger,
    #[error("frost: {0}")]
    Frost(#[from] crate::frost::Error),
    #[error(transparent)]
    Vault(#[from] VaultError),
    #[error("seats disagree about the vault; someone sent inconsistent data")]
    Disagreement,
    #[error("malformed message: {0}")]
    Malformed(String),
    #[error("called out of order")]
    OutOfOrder,
}

fn context(room: &str, round: u8) -> Vec<u8> {
    format!("tacit/{room}/{round}").into_bytes()
}

/// Shared plumbing: who's in the room and how to seal and open for them.
struct Room {
    key: SeatKey,
    id: String,
    seats: Vec<SeatPub>,
    me: usize,
}

impl Room {
    fn others(&self) -> impl Iterator<Item = (usize, &SeatPub)> {
        self.seats.iter().enumerate().filter(move |(i, _)| *i != self.me)
    }

    fn seal(&self, to: &SeatPub, round: u8, body: &[u8]) -> Outgoing {
        Outgoing { to: *to, round, body: self.key.seal(to, &context(&self.id, round), body) }
    }

    fn index_of(&self, pk: &SeatPub) -> Result<usize, SessionError> {
        self.seats.iter().position(|s| s == pk).ok_or(SessionError::Stranger)
    }

    /// Opens every message for `round`, keyed by sender seat index.
    fn open_round(&self, msgs: &[Incoming], round: u8) -> Result<BTreeMap<usize, Vec<u8>>, SessionError> {
        let mut out = BTreeMap::new();
        for m in msgs.iter().filter(|m| m.round == round) {
            let i = self.index_of(&m.from)?;
            let body = self
                .key
                .open(&m.from, &context(&self.id, round), &m.body)
                .map_err(|e| SessionError::Envelope(hex::encode(&m.from[..4]), e))?;
            out.insert(i, body);
        }
        Ok(out)
    }
}

/// What a seat keeps after key generation. The key package is secret.
pub struct VaultShare {
    pub seat: u16,
    pub key_package: KeyPackage,
    pub vault: Vault,
}

enum DkgState {
    Round1(round1::SecretPackage),
    Round2 { r1: BTreeMap<Identifier, round1::Package>, secret: round2::SecretPackage },
    AwaitViewingKey { kp: KeyPackage, pkp: PublicKeyPackage },
    AwaitConfirm { kp: KeyPackage, vault: Vault, digest: [u8; 32] },
    Done,
}

/// One seat's side of a 2 of 3 key generation.
pub struct DkgSession {
    room: Room,
    state: DkgState,
}

impl DkgSession {
    pub const ROUNDS: [u8; 4] = [1, 2, 3, 4];

    /// Starts key generation. `seats` lists all three seats' public keys in
    /// the room's order; seat numbers follow that order starting at 1.
    pub fn start(
        key: SeatKey,
        room_id: &str,
        seats: Vec<SeatPub>,
    ) -> Result<(Self, Vec<Outgoing>), SessionError> {
        if seats.len() != dkg::MAX_SIGNERS as usize {
            return Err(SessionError::Malformed("a vault needs exactly three seats".into()));
        }
        let me = seats.iter().position(|s| *s == key.public()).ok_or(SessionError::Stranger)?;
        let room = Room { key, id: room_id.into(), seats, me };
        let (secret, package) = dkg::start(seat(me as u16 + 1), OsRng)?;
        let bytes = package.serialize()?;
        let out = room.others().map(|(_, pk)| room.seal(pk, 1, &bytes)).collect();
        Ok((DkgSession { room, state: DkgState::Round1(secret) }, out))
    }

    pub fn seat(&self) -> u16 {
        self.room.me as u16 + 1
    }

    /// The round this session is waiting on, or `None` once finished.
    pub fn waiting_for(&self) -> Option<u8> {
        match self.state {
            DkgState::Round1(_) => Some(1),
            DkgState::Round2 { .. } => Some(2),
            DkgState::AwaitViewingKey { .. } => Some(3),
            DkgState::AwaitConfirm { .. } => Some(4),
            DkgState::Done => None,
        }
    }

    /// Feeds every message received so far. Returns messages to send and,
    /// when the session completes, this seat's share of the vault.
    pub fn step(&mut self, received: &[Incoming]) -> Result<(Vec<Outgoing>, Option<VaultShare>), SessionError> {
        let Some(round) = self.waiting_for() else { return Err(SessionError::OutOfOrder) };
        let msgs = self.room.open_round(received, round)?;
        // Seat 1 makes the viewing key itself, so it only waits on others in rounds 1, 2 and 4.
        let needed = match round {
            3 if self.room.me == 0 => 0,
            3 => 1,
            _ => self.room.seats.len() - 1,
        };
        if msgs.len() < needed {
            return Ok((vec![], None));
        }

        match std::mem::replace(&mut self.state, DkgState::Done) {
            DkgState::Round1(secret) => {
                let r1 = msgs
                    .iter()
                    .map(|(i, b)| Ok((seat(*i as u16 + 1), round1::Package::deserialize(b)?)))
                    .collect::<Result<BTreeMap<_, _>, SessionError>>()?;
                let (secret, outbox) = dkg::share(secret, &r1)?;
                let mut out = Vec::new();
                for (i, pk) in self.room.others() {
                    let pkg = outbox.get(&seat(i as u16 + 1)).ok_or(SessionError::OutOfOrder)?;
                    out.push(self.room.seal(pk, 2, &pkg.serialize()?));
                }
                self.state = DkgState::Round2 { r1, secret };
                Ok((out, None))
            }
            DkgState::Round2 { r1, secret } => {
                let r2 = msgs
                    .iter()
                    .map(|(i, b)| Ok((seat(*i as u16 + 1), round2::Package::deserialize(b)?)))
                    .collect::<Result<BTreeMap<_, _>, SessionError>>()?;
                let (kp, pkp) = dkg::finish(&secret, &r1, &r2)?;
                if self.room.me == 0 {
                    let vault = Vault::create(pkp, OsRng)?;
                    let fvk = vault.fvk().to_bytes();
                    let mut out: Vec<_> = self.room.others().map(|(_, pk)| self.room.seal(pk, 3, &fvk)).collect();
                    let digest = vault_digest(&vault)?;
                    out.extend(self.room.others().map(|(_, pk)| self.room.seal(pk, 4, &digest)));
                    self.state = DkgState::AwaitConfirm { kp, vault, digest };
                    Ok((out, None))
                } else {
                    self.state = DkgState::AwaitViewingKey { kp, pkp };
                    // The viewing key may already be here.
                    self.step(received)
                }
            }
            DkgState::AwaitViewingKey { kp, pkp } => {
                let fvk: [u8; 96] = msgs
                    .get(&0)
                    .ok_or(SessionError::Malformed("viewing key must come from seat 1".into()))?
                    .as_slice()
                    .try_into()
                    .map_err(|_| SessionError::Malformed("viewing key is not 96 bytes".into()))?;
                let vault = Vault::from_parts(pkp, &fvk)?;
                let digest = vault_digest(&vault)?;
                let out = self.room.others().map(|(_, pk)| self.room.seal(pk, 4, &digest)).collect();
                self.state = DkgState::AwaitConfirm { kp, vault, digest };
                Ok((out, None))
            }
            DkgState::AwaitConfirm { kp, vault, digest } => {
                if msgs.values().any(|d| d.as_slice() != digest) {
                    return Err(SessionError::Disagreement);
                }
                let share = VaultShare { seat: self.seat(), key_package: kp, vault };
                Ok((vec![], Some(share)))
            }
            DkgState::Done => Err(SessionError::OutOfOrder),
        }
    }
}

/// Fingerprint of everything the seats must agree on.
fn vault_digest(vault: &Vault) -> Result<[u8; 32], SessionError> {
    let mut h = Params::new().hash_length(32).personal(b"TacitVaultConfrm").to_state();
    h.update(&vault.public_key_package().serialize()?);
    h.update(&vault.fvk().to_bytes());
    Ok(h.finalize().as_bytes().try_into().unwrap())
}

// Signing rounds.
const SIGN_PCZT: u8 = 10;
const SIGN_COMMIT: u8 = 11;
const SIGN_PACKAGE: u8 = 12;
const SIGN_SHARES: u8 = 13;

fn encode_list<T>(items: &[T], ser: impl Fn(&T) -> Result<Vec<u8>, SessionError>) -> Result<Vec<u8>, SessionError> {
    let mut out = Vec::new();
    out.extend_from_slice(&(items.len() as u32).to_le_bytes());
    for it in items {
        let b = ser(it)?;
        out.extend_from_slice(&(b.len() as u32).to_le_bytes());
        out.extend_from_slice(&b);
    }
    Ok(out)
}

fn decode_list(mut b: &[u8]) -> Result<Vec<&[u8]>, SessionError> {
    let bad = || SessionError::Malformed("bad list encoding".into());
    let take = |b: &mut &[u8], n: usize| -> Result<Vec<u8>, SessionError> {
        if b.len() < n {
            return Err(bad());
        }
        let (h, t) = b.split_at(n);
        *b = t;
        Ok(h.to_vec())
    };
    let n = u32::from_le_bytes(take(&mut b, 4)?.try_into().unwrap()) as usize;
    let mut out = Vec::with_capacity(n);
    let mut rest = b;
    for _ in 0..n {
        if rest.len() < 4 {
            return Err(bad());
        }
        let len = u32::from_le_bytes(rest[..4].try_into().unwrap()) as usize;
        rest = &rest[4..];
        if rest.len() < len {
            return Err(bad());
        }
        out.push(&rest[..len]);
        rest = &rest[len..];
    }
    Ok(out)
}

/// The coordinator side of a signing session: hands the PCZT to the chosen
/// signers, collects their commitments and shares, and aggregates. It may or
/// may not be a signer itself.
pub struct Coordinator {
    room: Room,
    pczt: Pczt,
    vault: Vault,
    signers: Vec<usize>,
    own: Option<(KeyPackage, Option<Round1>)>,
    commitments: Option<BTreeMap<Identifier, Vec<SigningCommitments>>>,
}

impl Coordinator {
    /// `signers` are seat numbers (1 based). If the coordinator's own seat is
    /// among them, pass its key package.
    pub fn start(
        key: SeatKey,
        room_id: &str,
        seats: Vec<SeatPub>,
        vault: Vault,
        pczt: Pczt,
        signers: &[u16],
        own_key: Option<KeyPackage>,
    ) -> Result<(Self, Vec<Outgoing>), SessionError> {
        let me = seats.iter().position(|s| *s == key.public()).ok_or(SessionError::Stranger)?;
        let room = Room { key, id: room_id.into(), seats, me };
        let signers: Vec<usize> = signers.iter().map(|n| *n as usize - 1).collect();
        if signers.len() < dkg::MIN_SIGNERS as usize || signers.iter().any(|i| *i >= room.seats.len()) {
            return Err(SessionError::Malformed("need at least two valid signer seats".into()));
        }
        let own = match (signers.contains(&me), own_key) {
            (true, Some(kp)) => {
                let r1 = crate::commit(&pczt, &kp, OsRng)?;
                Some((kp, Some(r1)))
            }
            (true, None) => return Err(SessionError::Malformed("coordinator signs but gave no key".into())),
            (false, _) => None,
        };
        let bytes = pczt.clone().serialize().map_err(|e| SessionError::Malformed(format!("pczt: {e:?}")))?;
        let out = signers
            .iter()
            .filter(|i| **i != me)
            .map(|i| room.seal(&room.seats[*i], SIGN_PCZT, &bytes))
            .collect();
        Ok((Coordinator { room, pczt, vault, signers, own, commitments: None }, out))
    }

    /// Feeds received messages. Returns messages to send and, at the end,
    /// the fully authorized PCZT.
    pub fn step(&mut self, received: &[Incoming]) -> Result<(Vec<Outgoing>, Option<Pczt>), SessionError> {
        let remote: Vec<usize> = self.signers.iter().copied().filter(|i| *i != self.room.me).collect();
        if self.commitments.is_none() {
            let got = self.room.open_round(received, SIGN_COMMIT)?;
            if !remote.iter().all(|i| got.contains_key(i)) {
                return Ok((vec![], None));
            }
            let mut all = BTreeMap::new();
            for i in &remote {
                let list = decode_list(&got[i])?
                    .into_iter()
                    .map(SigningCommitments::deserialize)
                    .collect::<Result<Vec<_>, _>>()?;
                all.insert(seat(*i as u16 + 1), list);
            }
            if let Some((_, Some(r1))) = &self.own {
                all.insert(seat(self.room.me as u16 + 1), r1.commitments.clone());
            }
            let pkg = encode_commitments(&all)?;
            let out = remote.iter().map(|i| self.room.seal(&self.room.seats[*i], SIGN_PACKAGE, &pkg)).collect();
            self.commitments = Some(all);
            return Ok((out, None));
        }

        let got = self.room.open_round(received, SIGN_SHARES)?;
        if !remote.iter().all(|i| got.contains_key(i)) {
            return Ok((vec![], None));
        }
        let commitments = self.commitments.clone().unwrap();
        let mut shares = BTreeMap::new();
        for i in &remote {
            let list = decode_list(&got[i])?
                .into_iter()
                .map(SignatureShare::deserialize)
                .collect::<Result<Vec<_>, _>>()?;
            shares.insert(seat(*i as u16 + 1), list);
        }
        if let Some((kp, r1)) = &mut self.own {
            let r1 = r1.take().ok_or(SessionError::OutOfOrder)?;
            shares.insert(seat(self.room.me as u16 + 1), crate::sign(&self.pczt, kp, r1, &commitments)?);
        }
        let signed = crate::aggregate(self.pczt.clone(), &self.vault, &commitments, &shares)?;
        Ok((vec![], Some(signed)))
    }
}

fn encode_commitments(all: &BTreeMap<Identifier, Vec<SigningCommitments>>) -> Result<Vec<u8>, SessionError> {
    let entries: Vec<(Vec<u8>, Vec<u8>)> = all
        .iter()
        .map(|(id, list)| Ok((id.serialize(), encode_list(list, |c| Ok(c.serialize()?))?)))
        .collect::<Result<_, SessionError>>()?;
    encode_list(&entries, |(id, list)| {
        let mut b = Vec::new();
        b.extend_from_slice(&(id.len() as u32).to_le_bytes());
        b.extend_from_slice(id);
        b.extend_from_slice(list);
        Ok(b)
    })
}

fn decode_commitments(b: &[u8]) -> Result<BTreeMap<Identifier, Vec<SigningCommitments>>, SessionError> {
    let mut all = BTreeMap::new();
    for entry in decode_list(b)? {
        if entry.len() < 4 {
            return Err(SessionError::Malformed("bad commitment entry".into()));
        }
        let n = u32::from_le_bytes(entry[..4].try_into().unwrap()) as usize;
        let (id, list) = entry[4..].split_at(n.min(entry.len() - 4));
        let id = Identifier::deserialize(id)?;
        let list = decode_list(list)?
            .into_iter()
            .map(SigningCommitments::deserialize)
            .collect::<Result<Vec<_>, _>>()?;
        all.insert(id, list);
    }
    Ok(all)
}

/// A signer's side of a signing session.
pub struct Signer {
    room: Room,
    key_package: KeyPackage,
    coordinator: SeatPub,
    pczt: Option<Pczt>,
    round1: Option<Round1>,
    done: bool,
}

impl Signer {
    pub fn new(key: SeatKey, room_id: &str, seats: Vec<SeatPub>, coordinator: SeatPub, key_package: KeyPackage) -> Result<Self, SessionError> {
        let me = seats.iter().position(|s| *s == key.public()).ok_or(SessionError::Stranger)?;
        let room = Room { key, id: room_id.into(), seats, me };
        room.index_of(&coordinator)?;
        Ok(Signer { room, key_package, coordinator, pczt: None, round1: None, done: false })
    }

    /// The transaction this signer is being asked to authorize, once it has
    /// arrived, so the caller can show it to the user before approving.
    pub fn request(&self) -> Option<Result<SigningRequest, SessionError>> {
        self.pczt.as_ref().map(|p| SigningRequest::from_pczt(p).map_err(SessionError::from))
    }

    pub fn pczt(&self) -> Option<&Pczt> {
        self.pczt.as_ref()
    }

    pub fn is_done(&self) -> bool {
        self.done
    }

    /// Feeds received messages. `approve` is asked once, with the PCZT, before
    /// this seat commits to signing anything.
    pub fn step(
        &mut self,
        received: &[Incoming],
        approve: impl FnOnce(&Pczt) -> bool,
    ) -> Result<Vec<Outgoing>, SessionError> {
        if self.done {
            return Ok(vec![]);
        }
        let from_coord = |round| -> Result<Option<Vec<u8>>, SessionError> {
            let c = self.room.index_of(&self.coordinator)?;
            Ok(self.room.open_round(received, round)?.remove(&c))
        };
        if self.round1.is_none() {
            let Some(bytes) = from_coord(SIGN_PCZT)? else { return Ok(vec![]) };
            let pczt = Pczt::parse(&bytes).map_err(|e| SessionError::Malformed(format!("pczt: {e:?}")))?;
            if !approve(&pczt) {
                self.done = true;
                return Ok(vec![]);
            }
            let r1 = crate::commit(&pczt, &self.key_package, OsRng)?;
            let body = encode_list(&r1.commitments, |c| Ok(c.serialize()?))?;
            self.pczt = Some(pczt);
            self.round1 = Some(r1);
            return Ok(vec![self.room.seal(&self.coordinator, SIGN_COMMIT, &body)]);
        }
        let Some(bytes) = from_coord(SIGN_PACKAGE)? else { return Ok(vec![]) };
        let commitments = decode_commitments(&bytes)?;
        let r1 = self.round1.take().ok_or(SessionError::OutOfOrder)?;
        let shares = crate::sign(self.pczt.as_ref().unwrap(), &self.key_package, r1, &commitments)?;
        let body = encode_list(&shares, |s| Ok(s.serialize()))?;
        self.done = true;
        Ok(vec![self.room.seal(&self.coordinator, SIGN_SHARES, &body)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Delivers every outgoing message to its recipient's inbox, as a
    /// coordinator would, and runs all three seats to completion.
    #[test]
    fn three_seats_agree_on_one_vault() {
        let keys: Vec<SeatKey> = (0..3).map(|_| SeatKey::generate()).collect();
        let seats: Vec<SeatPub> = keys.iter().map(|k| k.public()).collect();
        let mut inbox: BTreeMap<SeatPub, Vec<Incoming>> = BTreeMap::new();
        let mut sessions = Vec::new();
        for k in keys {
            let me = k.public();
            let (s, out) = DkgSession::start(k, "room-1", seats.clone()).unwrap();
            for o in out {
                inbox.entry(o.to).or_default().push(Incoming { from: me, round: o.round, body: o.body });
            }
            sessions.push(s);
        }
        let mut shares: Vec<Option<VaultShare>> = (0..3).map(|_| None).collect();
        for _ in 0..10 {
            for (i, s) in sessions.iter_mut().enumerate() {
                if shares[i].is_some() {
                    continue;
                }
                let me = seats[i];
                let mine = inbox.get(&me).cloned().unwrap_or_default();
                let (out, done) = s.step(&mine).unwrap();
                for o in out {
                    inbox.entry(o.to).or_default().push(Incoming { from: me, round: o.round, body: o.body });
                }
                shares[i] = done;
            }
        }
        let shares: Vec<VaultShare> = shares.into_iter().map(|s| s.expect("every seat finishes")).collect();
        let fvk = shares[0].vault.fvk().to_bytes();
        assert!(shares.iter().all(|s| s.vault.fvk().to_bytes() == fvk));
        assert_eq!(shares.iter().map(|s| s.seat).collect::<Vec<_>>(), vec![1, 2, 3]);
    }
}
