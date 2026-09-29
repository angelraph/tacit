//! Timelock sealing against the drand quicknet beacon.
//!
//! A sealed box can only be opened once drand publishes the signature for its
//! target round. Nobody holds a key before that, including whoever sealed it.
//!
//! tlock itself only encrypts 16 bytes, so we tlock a random 16 byte file key
//! and use it (through HKDF) to encrypt the real payload with
//! ChaCha20-Poly1305. The round number and the tlock ciphertext are bound in
//! as associated data so neither can be swapped out.
//!
//! Wire format:
//!
//! ```text
//! round (u64 BE, 8) | tlock ciphertext (128) | aead ciphertext (payload + 16)
//! ```

use ark_bls12_381::{Bls12_381, Fr, G1Affine, G2Affine};
use ark_ec::pairing::Pairing;
use ark_ec::{AffineRepr, CurveGroup};
use ark_ff::PrimeField;
use ark_serialize::{CanonicalDeserialize, CanonicalSerialize};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::ChaCha20Poly1305;
use hkdf::Hkdf;
use rand_core::{OsRng, RngCore};
use sha2::Sha256;

use crate::beacon::Beacon;

/// drand quicknet chain hash.
pub const QUICKNET_CHAIN_HASH: &str =
    "52db9ba70e0cc0f6eaf7803dd07447a1f5477735fd3f661792ba94600c84e971";
/// drand quicknet group public key (G2, compressed).
pub const QUICKNET_PUBLIC_KEY: &str = "83cf0f2896adee7eb8b5f01fcad3912212c437e0073e911fb90022d3e760183c8c4b450b6a0a6c3ac6a5776a2d1064510d1fec758c921cc22b0e17e63aaf4bcb5ed66304de9cf809bd274ca73bab4af5a6e9c76a4bc09e76eae8991ef5ece45a";
pub const QUICKNET_GENESIS: u64 = 1_692_803_367;
pub const QUICKNET_PERIOD: u64 = 3;

const TLOCK_CT_LEN: usize = 96 + 16 + 16;
const TAG_LEN: usize = 16;
const HEADER_LEN: usize = 8 + TLOCK_CT_LEN;
const HKDF_SALT: &[u8] = b"tacit/seal/v1";

#[derive(Debug, thiserror::Error)]
pub enum SealError {
    #[error("sealed box is too short")]
    Truncated,
    #[error("timelock encryption failed: {0}")]
    Tlock(String),
    #[error("box is sealed until round {sealed_until}, beacon is for round {beacon}")]
    WrongRound { sealed_until: u64, beacon: u64 },
    #[error("timelock ciphertext is malformed")]
    Malformed,
    #[error("payload failed authentication")]
    Tampered,
}

/// First quicknet round whose beacon is published at or after `unix_secs`.
pub fn round_at(unix_secs: u64) -> u64 {
    if unix_secs <= QUICKNET_GENESIS {
        return 1;
    }
    let elapsed = unix_secs - QUICKNET_GENESIS;
    elapsed.div_ceil(QUICKNET_PERIOD) + 1
}

/// Unix time at which `round` is published.
pub fn round_time(round: u64) -> u64 {
    QUICKNET_GENESIS + round.saturating_sub(1) * QUICKNET_PERIOD
}

/// Round number a sealed box targets, readable without opening it.
pub fn target_round(sealed: &[u8]) -> Result<u64, SealError> {
    let head: [u8; 8] = sealed.get(..8).ok_or(SealError::Truncated)?.try_into().unwrap();
    Ok(u64::from_be_bytes(head))
}

/// Size in bytes of a sealed box carrying `payload_len` bytes.
pub const fn sealed_len(payload_len: usize) -> usize {
    HEADER_LEN + payload_len + TAG_LEN
}

pub fn seal(payload: &[u8], round: u64) -> Result<Vec<u8>, SealError> {
    let pk = hex::decode(QUICKNET_PUBLIC_KEY).expect("constant is valid hex");

    let mut file_key = [0u8; 16];
    OsRng.fill_bytes(&mut file_key);

    let mut out = Vec::with_capacity(sealed_len(payload.len()));
    out.extend_from_slice(&round.to_be_bytes());
    tlock::encrypt(&mut out, &file_key[..], &pk, round)
        .map_err(|e| SealError::Tlock(e.to_string()))?;
    debug_assert_eq!(out.len(), HEADER_LEN);

    let cipher = payload_cipher(&file_key, round);
    let ct = cipher
        .encrypt(&zero_nonce(), Payload { msg: payload, aad: &out })
        .map_err(|_| SealError::Tlock("aead encrypt".into()))?;
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Open a sealed box with the verified drand beacon for its target round.
pub fn open(sealed: &[u8], beacon: &Beacon) -> Result<Vec<u8>, SealError> {
    if sealed.len() < HEADER_LEN + TAG_LEN {
        return Err(SealError::Truncated);
    }
    let round = target_round(sealed)?;
    if round != beacon.round() {
        return Err(SealError::WrongRound { sealed_until: round, beacon: beacon.round() });
    }
    let (header, ct) = sealed.split_at(HEADER_LEN);
    let file_key = ibe_decrypt(&header[8..], beacon.signature())?;

    payload_cipher(&file_key, round)
        .decrypt(&zero_nonce(), Payload { msg: ct, aad: header })
        .map_err(|_| SealError::Tampered)
}

/// Boneh-Franklin decryption as used by tlock (signatures on G1, U on G2).
///
/// This mirrors `tlock::decrypt`, but returns an error rather than panicking
/// on a bad ciphertext, since sealed bids come from untrusted senders.
fn ibe_decrypt(ct: &[u8], signature: &[u8]) -> Result<[u8; 16], SealError> {
    use sha2::Digest;

    let (u_bytes, rest) = ct.split_at(96);
    let (v, w) = rest.split_at(16);
    let u = G2Affine::deserialize_compressed(u_bytes).map_err(|_| SealError::Malformed)?;
    let sig = G1Affine::deserialize_compressed(signature).map_err(|_| SealError::Malformed)?;

    let mut gt = Vec::new();
    Bls12_381::pairing(sig, u)
        .serialize_compressed(&mut gt)
        .map_err(|_| SealError::Malformed)?;
    gt.reverse();
    let h2 = Sha256::new().chain_update(b"IBE-H2").chain_update(&gt).finalize();
    let sigma: [u8; 16] = xor16(&h2[..16], v);

    let h4 = Sha256::new().chain_update(b"IBE-H4").chain_update(sigma).finalize();
    let msg = xor16(&h4[..16], w);

    // Recompute r and check U = r * G2, which authenticates sigma and msg.
    let h3 = Sha256::new()
        .chain_update(b"IBE-H3")
        .chain_update(sigma)
        .chain_update(msg)
        .finalize();
    let r = expand_to_scalar(&h3).ok_or(SealError::Malformed)?;
    if (G2Affine::generator() * r).into_affine() != u {
        return Err(SealError::Tampered);
    }
    Ok(msg)
}

/// drand's "expand_message": hash H(i || msg) with the top bit cleared until
/// the result is a canonical scalar.
fn expand_to_scalar(msg: &[u8]) -> Option<Fr> {
    use sha2::Digest;
    for i in 1..u16::MAX {
        let mut h: [u8; 32] = Sha256::new()
            .chain_update(i.to_le_bytes())
            .chain_update(msg)
            .finalize()
            .into();
        h[0] >>= 1;
        h.reverse();
        if Fr::deserialize_compressed(&h[..]).is_ok() {
            return Some(Fr::from_le_bytes_mod_order(&h));
        }
    }
    None
}

fn xor16(a: &[u8], b: &[u8]) -> [u8; 16] {
    let mut out = [0u8; 16];
    for (o, (x, y)) in out.iter_mut().zip(a.iter().zip(b)) {
        *o = x ^ y;
    }
    out
}

fn payload_cipher(file_key: &[u8; 16], round: u64) -> ChaCha20Poly1305 {
    let hk = Hkdf::<Sha256>::new(Some(HKDF_SALT), file_key);
    let mut key = [0u8; 32];
    hk.expand(&round.to_be_bytes(), &mut key)
        .expect("32 bytes is a valid HKDF output length");
    ChaCha20Poly1305::new(&key.into())
}

// Each file key is fresh and used for exactly one message, so a fixed nonce is safe.
fn zero_nonce() -> chacha20poly1305::Nonce {
    Default::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real quicknet beacon, round 32633680, fetched from api.drand.sh.
    const ROUND: u64 = 32_633_680;
    const SIG: &str = "980de8271fc02d5191a0e8977922c347f5dbad595e2d080117ab64385a341b79cfb7ebef0459b5260209dc77259d8121";

    fn beacon() -> Beacon {
        Beacon::verify(ROUND, &hex::decode(SIG).unwrap()).unwrap()
    }

    #[test]
    fn opens_with_the_right_beacon() {
        let payload = b"bid: 12.5 ZEC";
        let sealed = seal(payload, ROUND).unwrap();
        assert_eq!(sealed.len(), sealed_len(payload.len()));
        assert_eq!(target_round(&sealed).unwrap(), ROUND);
        assert_eq!(open(&sealed, &beacon()).unwrap(), payload);
    }

    #[test]
    fn refuses_a_different_round() {
        let sealed = seal(b"x", ROUND + 1).unwrap();
        assert!(matches!(open(&sealed, &beacon()), Err(SealError::WrongRound { .. })));
    }

    #[test]
    fn detects_tampering() {
        let mut sealed = seal(b"payload", ROUND).unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 1;
        assert!(matches!(open(&sealed, &beacon()), Err(SealError::Tampered)));
    }

    #[test]
    fn garbage_timelock_is_an_error_not_a_panic() {
        let mut sealed = seal(b"payload", ROUND).unwrap();
        for b in &mut sealed[8 + 96..8 + 128] {
            *b ^= 0x5a;
        }
        assert!(open(&sealed, &beacon()).is_err());
        sealed[9] ^= 0xff;
        assert!(open(&sealed, &beacon()).is_err());
    }

    #[test]
    fn file_keys_ending_in_zero_survive() {
        // tlock's own decrypt trims trailing zeros; ours must not.
        for _ in 0..64 {
            let sealed = seal(&[7u8; 40], ROUND).unwrap();
            assert_eq!(open(&sealed, &beacon()).unwrap(), [7u8; 40]);
        }
    }

    #[test]
    fn round_math_matches_the_beacon() {
        assert_eq!(round_at(round_time(ROUND)), ROUND);
        assert_eq!(round_at(round_time(ROUND) + 1), ROUND + 1);
    }
}
