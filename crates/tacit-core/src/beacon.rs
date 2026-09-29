//! Verification of drand quicknet beacons.
//!
//! Beacons usually come from a public HTTP relay we don't control, so every
//! one is checked against the quicknet group key before it's used to open
//! anything. Quicknet is unchained: the signed message is sha256(round).

use ark_bls12_381::{g1, Bls12_381, G1Affine, G2Affine};
use ark_ec::hashing::curve_maps::wb::WBMap;
use ark_ec::hashing::map_to_curve_hasher::MapToCurveBasedHasher;
use ark_ec::hashing::HashToCurve;
use ark_ec::models::short_weierstrass::Projective;
use ark_ec::pairing::Pairing;
use ark_ec::{AffineRepr, CurveGroup};
use ark_ff::field_hashers::DefaultFieldHasher;
use ark_ff::Zero;
use ark_serialize::CanonicalDeserialize;
use sha2::{Digest, Sha256};

use crate::seal::QUICKNET_PUBLIC_KEY;

const DST: &[u8] = b"BLS_SIG_BLS12381G1_XMD:SHA-256_SSWU_RO_NUL_";

/// A quicknet beacon that has passed signature verification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Beacon {
    round: u64,
    signature: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum BeaconError {
    #[error("signature is not a valid G1 point")]
    Malformed,
    #[error("signature does not verify for round {0}")]
    Invalid(u64),
}

impl Beacon {
    pub fn verify(round: u64, signature: &[u8]) -> Result<Self, BeaconError> {
        let sig = G1Affine::deserialize_compressed(signature).map_err(|_| BeaconError::Malformed)?;
        let pk_bytes = hex::decode(QUICKNET_PUBLIC_KEY).expect("constant is valid hex");
        let pk = G2Affine::deserialize_compressed(&pk_bytes[..]).expect("constant is a valid key");

        let msg = Sha256::digest(round.to_be_bytes());
        let hasher = MapToCurveBasedHasher::<
            Projective<g1::Config>,
            DefaultFieldHasher<Sha256, 128>,
            WBMap<g1::Config>,
        >::new(DST)
        .expect("valid DST");
        let h = hasher.hash(&msg).map_err(|_| BeaconError::Malformed)?;

        // e(sig, g2) == e(H(m), pk)  <=>  e(sig, -g2) * e(H(m), pk) == 1
        let g2 = G2Affine::generator();
        let lhs = Bls12_381::multi_pairing(
            [sig, h],
            [(-g2.into_group()).into_affine(), pk],
        );
        if lhs.is_zero() {
            Ok(Self { round, signature: signature.to_vec() })
        } else {
            Err(BeaconError::Invalid(round))
        }
    }

    pub fn round(&self) -> u64 {
        self.round
    }

    pub fn signature(&self) -> &[u8] {
        &self.signature
    }

    /// The beacon's public randomness, sha256(signature).
    pub fn randomness(&self) -> [u8; 32] {
        Sha256::digest(&self.signature).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROUND: u64 = 32_633_680;
    const SIG: &str = "980de8271fc02d5191a0e8977922c347f5dbad595e2d080117ab64385a341b79cfb7ebef0459b5260209dc77259d8121";
    const RANDOMNESS: &str = "f7d033bcfaec001554d1d703531f9db289484e2b80cc30440da0736e58b8bcca";

    #[test]
    fn accepts_a_real_beacon() {
        let b = Beacon::verify(ROUND, &hex::decode(SIG).unwrap()).unwrap();
        assert_eq!(hex::encode(b.randomness()), RANDOMNESS);
    }

    #[test]
    fn rejects_the_wrong_round() {
        assert!(matches!(
            Beacon::verify(ROUND + 1, &hex::decode(SIG).unwrap()),
            Err(BeaconError::Invalid(_))
        ));
    }
}
