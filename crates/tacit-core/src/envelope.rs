//! Sealed messages between vault seats.
//!
//! Seats talk through the coordinator, which only stores and forwards. During
//! key generation they exchange secret key shares, so nothing they send may
//! be readable or forgeable by the coordinator.
//!
//! Each seat has a long-term X25519 key that identifies it. A message mixes
//! two Diffie-Hellman results: one from a fresh ephemeral key (so every
//! message has its own key) and one from the sender's long-term key (so the
//! recipient knows who sent it). The session context and both seats' public
//! keys are bound in, so a message can't be replayed into another session or
//! round, or bounced back at its sender.
//!
//! Wire format: `ephemeral public key (32) | nonce (24) | ciphertext + tag`.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use rand_core::{OsRng, RngCore};
use sha2::Sha256;
use x25519_dalek::{EphemeralSecret, PublicKey, StaticSecret};

const HEADER: usize = 32 + 24;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EnvelopeError {
    #[error("envelope is too short")]
    Truncated,
    #[error("envelope doesn't open with these keys and context")]
    Rejected,
}

/// A seat's long-term identity.
pub struct SeatKey {
    secret: StaticSecret,
    public: PublicKey,
}

impl SeatKey {
    pub fn generate() -> Self {
        let secret = StaticSecret::random_from_rng(OsRng);
        let public = PublicKey::from(&secret);
        SeatKey { secret, public }
    }

    pub fn from_bytes(secret: [u8; 32]) -> Self {
        let secret = StaticSecret::from(secret);
        let public = PublicKey::from(&secret);
        SeatKey { secret, public }
    }

    pub fn to_bytes(&self) -> [u8; 32] {
        self.secret.to_bytes()
    }

    pub fn public(&self) -> [u8; 32] {
        self.public.to_bytes()
    }

    /// Seals `plaintext` so that only `to` can read it and knows it came from us.
    pub fn seal(&self, to: &[u8; 32], context: &[u8], plaintext: &[u8]) -> Vec<u8> {
        let to = PublicKey::from(*to);
        let eph = EphemeralSecret::random_from_rng(OsRng);
        let eph_pub = PublicKey::from(&eph);
        let es = eph.diffie_hellman(&to);
        let ss = self.secret.diffie_hellman(&to);
        let cipher = cipher(es.as_bytes(), ss.as_bytes(), &eph_pub, &self.public, &to, context);

        let mut nonce = [0u8; 24];
        OsRng.fill_bytes(&mut nonce);
        let aad = aad(&self.public, &to, context);
        let ct = cipher
            .encrypt(XNonce::from_slice(&nonce), Payload { msg: plaintext, aad: &aad })
            .expect("encryption with a valid key can't fail");

        let mut out = Vec::with_capacity(HEADER + ct.len());
        out.extend_from_slice(eph_pub.as_bytes());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        out
    }

    /// Opens an envelope that `from` sealed for us under `context`.
    pub fn open(&self, from: &[u8; 32], context: &[u8], envelope: &[u8]) -> Result<Vec<u8>, EnvelopeError> {
        if envelope.len() < HEADER + 16 {
            return Err(EnvelopeError::Truncated);
        }
        let from = PublicKey::from(*from);
        let eph_pub = PublicKey::from(<[u8; 32]>::try_from(&envelope[..32]).unwrap());
        let nonce = XNonce::from_slice(&envelope[32..HEADER]);
        let es = self.secret.diffie_hellman(&eph_pub);
        let ss = self.secret.diffie_hellman(&from);
        let cipher = cipher(es.as_bytes(), ss.as_bytes(), &eph_pub, &from, &self.public, context);
        let aad = aad(&from, &self.public, context);
        cipher
            .decrypt(nonce, Payload { msg: &envelope[HEADER..], aad: &aad })
            .map_err(|_| EnvelopeError::Rejected)
    }
}

fn cipher(
    es: &[u8; 32],
    ss: &[u8; 32],
    eph: &PublicKey,
    sender: &PublicKey,
    recipient: &PublicKey,
    context: &[u8],
) -> XChaCha20Poly1305 {
    let mut ikm = [0u8; 64];
    ikm[..32].copy_from_slice(es);
    ikm[32..].copy_from_slice(ss);
    let mut info = Vec::with_capacity(96 + context.len());
    info.extend_from_slice(eph.as_bytes());
    info.extend_from_slice(sender.as_bytes());
    info.extend_from_slice(recipient.as_bytes());
    info.extend_from_slice(context);
    let mut key = [0u8; 32];
    Hkdf::<Sha256>::new(Some(b"tacit/envelope/v1"), &ikm)
        .expand(&info, &mut key)
        .expect("32 bytes is a valid HKDF output length");
    XChaCha20Poly1305::new(&key.into())
}

fn aad(sender: &PublicKey, recipient: &PublicKey, context: &[u8]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(64 + context.len());
    aad.extend_from_slice(sender.as_bytes());
    aad.extend_from_slice(recipient.as_bytes());
    aad.extend_from_slice(context);
    aad
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_recipient_can_open() {
        let (alice, bob, eve) = (SeatKey::generate(), SeatKey::generate(), SeatKey::generate());
        let env = alice.seal(&bob.public(), b"session 1 / dkg round 2", b"secret share");
        assert_eq!(bob.open(&alice.public(), b"session 1 / dkg round 2", &env).unwrap(), b"secret share");
        assert_eq!(eve.open(&alice.public(), b"session 1 / dkg round 2", &env), Err(EnvelopeError::Rejected));
    }

    #[test]
    fn a_forged_sender_is_rejected() {
        let (alice, bob, eve) = (SeatKey::generate(), SeatKey::generate(), SeatKey::generate());
        // Eve seals to Bob and claims to be Alice.
        let env = eve.seal(&bob.public(), b"ctx", b"take the money");
        assert_eq!(bob.open(&alice.public(), b"ctx", &env), Err(EnvelopeError::Rejected));
    }

    #[test]
    fn context_and_direction_are_bound() {
        let (alice, bob) = (SeatKey::generate(), SeatKey::generate());
        let env = alice.seal(&bob.public(), b"round 1", b"m");
        assert!(bob.open(&alice.public(), b"round 2", &env).is_err(), "replayed into another round");
        assert!(alice.open(&bob.public(), b"round 1", &env).is_err(), "reflected back at the sender");
    }

    #[test]
    fn tampering_is_detected() {
        let (alice, bob) = (SeatKey::generate(), SeatKey::generate());
        let mut env = alice.seal(&bob.public(), b"ctx", b"hello");
        let last = env.len() - 1;
        env[last] ^= 1;
        assert!(bob.open(&alice.public(), b"ctx", &env).is_err());
        assert_eq!(bob.open(&alice.public(), b"ctx", &env[..10]), Err(EnvelopeError::Truncated));
    }

    #[test]
    fn keys_round_trip() {
        let k = SeatKey::generate();
        assert_eq!(SeatKey::from_bytes(k.to_bytes()).public(), k.public());
    }
}
