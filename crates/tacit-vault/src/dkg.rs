//! Distributed key generation for a vault.
//!
//! Each participant runs these three steps on their own device and only ever
//! sends the packages marked for others. The group secret is never assembled
//! anywhere. The redpallas ciphersuite negates the result when needed so the
//! group key has an even Y coordinate, as Ironwood spend keys require.

use std::collections::BTreeMap;

use rand_core::{CryptoRng, RngCore};

use crate::frost::keys::dkg::{self, round1, round2};
use crate::frost::keys::{KeyPackage, PublicKeyPackage};
use crate::frost::{Error, Identifier};

/// Tacit vaults: any 2 of 3 can spend.
pub const MAX_SIGNERS: u16 = 3;
pub const MIN_SIGNERS: u16 = 2;

/// Participant identifier for seat `n` (1, 2 or 3).
pub fn seat(n: u16) -> Identifier {
    Identifier::try_from(n).expect("seat numbers are nonzero")
}

/// Step one: commit to a random polynomial. Broadcast the returned package.
pub fn start(
    me: Identifier,
    rng: impl RngCore + CryptoRng,
) -> Result<(round1::SecretPackage, round1::Package), Error> {
    dkg::part1(me, MAX_SIGNERS, MIN_SIGNERS, rng)
}

/// Step two: given everyone else's broadcast, produce one private package
/// per other participant. Those must travel over an encrypted channel.
pub fn share(
    secret: round1::SecretPackage,
    others: &BTreeMap<Identifier, round1::Package>,
) -> Result<(round2::SecretPackage, BTreeMap<Identifier, round2::Package>), Error> {
    dkg::part2(secret, others)
}

/// Step three: combine the packages addressed to us into our long lived key
/// share and the public key package everyone should agree on.
pub fn finish(
    secret: &round2::SecretPackage,
    round1_from_others: &BTreeMap<Identifier, round1::Package>,
    round2_for_me: &BTreeMap<Identifier, round2::Package>,
) -> Result<(KeyPackage, PublicKeyPackage), Error> {
    dkg::part3(secret, round1_from_others, round2_for_me)
}
