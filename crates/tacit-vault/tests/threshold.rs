//! Runs the full 3 party DKG and 2 of 3 rerandomized signing locally, then
//! checks the result the way a Zcash node would: against the randomized
//! spend validating key rk = ak + [alpha]G.

use std::collections::BTreeMap;

use orchard::primitives::redpallas::{self, SpendAuth};
use rand::rngs::OsRng;
use tacit_vault::dkg::{self, seat};
use tacit_vault::frost::keys::{KeyPackage, PublicKeyPackage};
use tacit_vault::frost::{self, Identifier, SigningPackage};
use tacit_vault::Vault;
use zcash_protocol::consensus::Network;

fn run_dkg() -> (BTreeMap<Identifier, KeyPackage>, PublicKeyPackage) {
    let seats: Vec<Identifier> = (1..=3).map(seat).collect();

    let mut r1_secret = BTreeMap::new();
    let mut r1_public = BTreeMap::new();
    for id in &seats {
        let (s, p) = dkg::start(*id, OsRng).unwrap();
        r1_secret.insert(*id, s);
        r1_public.insert(*id, p);
    }
    fn others(
        me: &Identifier,
        all: &BTreeMap<Identifier, dkg_r1::Package>,
    ) -> BTreeMap<Identifier, dkg_r1::Package> {
        all.iter().filter(|(id, _)| *id != me).map(|(id, p)| (*id, p.clone())).collect()
    }

    let mut r2_secret = BTreeMap::new();
    let mut r2_outbox = BTreeMap::new();
    for id in &seats {
        let (s, out) = dkg::share(r1_secret.remove(id).unwrap(), &others(id, &r1_public)).unwrap();
        r2_secret.insert(*id, s);
        r2_outbox.insert(*id, out);
    }

    let mut keys = BTreeMap::new();
    let mut publics = Vec::new();
    for id in &seats {
        let inbox: BTreeMap<_, _> = r2_outbox
            .iter()
            .filter(|(from, _)| *from != id)
            .map(|(from, out)| (*from, out[id].clone()))
            .collect();
        let (kp, pkp) = dkg::finish(&r2_secret[id], &others(id, &r1_public), &inbox).unwrap();
        keys.insert(*id, kp);
        publics.push(pkp);
    }
    assert!(publics.windows(2).all(|w| w[0] == w[1]), "everyone agrees on the group key");
    (keys, publics.remove(0))
}

use tacit_vault::frost::keys::dkg::round1 as dkg_r1;

fn sign_2_of_3(
    keys: &BTreeMap<Identifier, KeyPackage>,
    public: &PublicKeyPackage,
    signers: [u16; 2],
    msg: &[u8; 32],
    alpha: &[u8; 32],
) -> [u8; 64] {
    let mut nonces = BTreeMap::new();
    let mut commitments = BTreeMap::new();
    for n in signers {
        let (nonce, c) = frost::round1::commit(keys[&seat(n)].signing_share(), &mut OsRng);
        nonces.insert(seat(n), nonce);
        commitments.insert(seat(n), c);
    }
    let package = SigningPackage::new(commitments, msg);
    let randomizer = frost::rerandomized::Randomizer::deserialize(alpha).unwrap();
    let shares: BTreeMap<_, _> = signers
        .iter()
        .map(|n| {
            let id = seat(*n);
            #[allow(deprecated)]
            let share = frost_rerandomized::sign(&package, &nonces[&id], &keys[&id], randomizer).unwrap();
            (id, share)
        })
        .collect();
    let params = frost::rerandomized::RandomizedParams::from_randomizer(public.verifying_key(), randomizer);
    frost::rerandomized::aggregate(&package, &shares, public, &params)
        .unwrap()
        .serialize()
        .unwrap()
        .try_into()
        .unwrap()
}

/// A valid Pallas scalar to stand in for a PCZT's alpha.
fn random_alpha() -> [u8; 32] {
    use pasta_curves::group::ff::{Field, PrimeField};
    pasta_curves::pallas::Scalar::random(OsRng).to_repr()
}

#[test]
fn any_two_of_three_can_authorize_an_ironwood_spend() {
    let (keys, public) = run_dkg();
    let vault = Vault::create(public.clone(), OsRng).unwrap();

    let addr = vault.encode_address(&Network::TestNetwork, 0);
    assert!(addr.starts_with("utest1"), "{addr}");
    assert_ne!(addr, vault.encode_address(&Network::TestNetwork, 1));

    let sighash = [0x42u8; 32];
    let alpha = random_alpha();
    let ak: [u8; 32] = vault.fvk().to_bytes()[..32].try_into().unwrap();
    let ak = redpallas::VerificationKey::<SpendAuth>::try_from(ak).unwrap();
    let alpha_scalar: pasta_curves::pallas::Scalar =
        pasta_curves::group::ff::PrimeField::from_repr(alpha).unwrap();
    let rk = ak.randomize(&alpha_scalar);

    for signers in [[1, 2], [1, 3], [2, 3]] {
        let sig = sign_2_of_3(&keys, &public, signers, &sighash, &alpha);
        rk.verify(&sighash, &redpallas::Signature::from(sig))
            .unwrap_or_else(|_| panic!("seats {signers:?} produced an invalid spend signature"));
    }
}

#[test]
fn viewing_key_round_trips_and_is_bound_to_the_group_key() {
    let (_, public) = run_dkg();
    let vault = Vault::create(public.clone(), OsRng).unwrap();
    let fvk = vault.fvk().to_bytes();
    let again = Vault::from_parts(public, &fvk).unwrap();
    assert_eq!(again.encode_address(&Network::TestNetwork, 0), vault.encode_address(&Network::TestNetwork, 0));

    let (_, other_public) = run_dkg();
    assert!(Vault::from_parts(other_public, &fvk).is_err(), "a viewing key can't be paired with another group");
}

#[test]
fn one_signer_is_not_enough() {
    let (keys, public) = run_dkg();
    let (nonce, c) = frost::round1::commit(keys[&seat(1)].signing_share(), &mut OsRng);
    let package = SigningPackage::new(BTreeMap::from([(seat(1), c)]), &[1u8; 32]);
    let randomizer = frost::rerandomized::Randomizer::deserialize(&random_alpha()).unwrap();
    // FROST refuses to produce a share for a package below the threshold.
    #[allow(deprecated)]
    let refused = frost_rerandomized::sign(&package, &nonce, &keys[&seat(1)], randomizer).is_err();
    assert!(refused);
    let _ = public;
}
