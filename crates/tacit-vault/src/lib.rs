//! Threshold vaults for Ironwood funds.
//!
//! A vault is an Ironwood address whose spend authorizing key is never held by
//! anyone. It exists only as FROST shares (for Tacit, 2 of 3), so moving funds
//! out needs a threshold of signers to cooperate on each transaction.
//!
//! Signing follows the PCZT flow. Whoever builds the transaction (the
//! coordinator) hands every signer the same PCZT. Each signer recomputes the
//! sighash and the per-spend randomizers from the PCZT on their own, so a
//! dishonest coordinator can't get a signature over anything other than the
//! transaction the signer actually looked at.

use std::collections::BTreeMap;

use orchard::keys::{FullViewingKey, SpendValidatingKey, SpendingKey};
use orchard::primitives::redpallas::{self, SpendAuth};
use pczt::roles::low_level_signer;
use pczt::roles::signer::Signer;
use pasta_curves::group::ff::PrimeField;
use pczt::Pczt;
use rand_core::{CryptoRng, RngCore};
use zcash_keys::address::UnifiedAddress;
use zcash_keys::keys::{UnifiedAddressRequest, UnifiedFullViewingKey};
use zcash_protocol::consensus::Parameters;

pub use reddsa::frost::redpallas as frost;
use frost::keys::{KeyPackage, PublicKeyPackage};
use frost::round1::{SigningCommitments, SigningNonces};
use frost::round2::SignatureShare;
use frost::{Identifier, SigningPackage};

pub mod dkg;

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("group key is not a valid Ironwood spend validating key")]
    InvalidGroupKey,
    #[error("viewing key does not belong to this group key")]
    ViewingKeyMismatch,
    #[error("frost: {0}")]
    Frost(#[from] frost::Error),
    #[error("pczt: {0:?}")]
    Pczt(String),
    #[error("expected {expected} signing commitments per signer, got {got}")]
    CommitmentCount { expected: usize, got: usize },
    #[error("the PCZT differs from the one this signer committed to")]
    TransactionChanged,
    #[error("spend {0} has no randomizer; was the PCZT built by an IO finalizer?")]
    MissingRandomizer(usize),
}

/// The public half of a vault: the FROST group key plus the Ironwood viewing
/// key built around it. Safe to share with every participant and, for
/// auditable markets, with the world.
#[derive(Clone, Debug)]
pub struct Vault {
    public: PublicKeyPackage,
    fvk: FullViewingKey,
}

impl Vault {
    /// Creates the viewing key for a freshly generated group key.
    ///
    /// Only one participant should call this. The resulting viewing key is
    /// then shared with the others through [`Vault::from_parts`], since the
    /// nullifier and address randomness in it are chosen at random here.
    pub fn create(
        public: PublicKeyPackage,
        mut rng: impl RngCore + CryptoRng,
    ) -> Result<Self, VaultError> {
        let ak = group_ak(&public)?;
        let sk = loop {
            let mut bytes = [0u8; 32];
            rng.fill_bytes(&mut bytes);
            if let Some(sk) = SpendingKey::from_bytes(bytes).into() {
                break sk;
            }
        };
        // Only nk and rivk come from sk here; spend authority is the FROST key.
        // See THREAT_MODEL.md on why vault keys are not quantum recoverable.
        let fvk =
            FullViewingKey::from_sk_ak_incompatible_with_quantum_recoverability_and_will_be_removed(
                &sk, ak,
            );
        Ok(Self { public, fvk })
    }

    /// Rebuilds a vault from its group key and the shared viewing key bytes,
    /// checking that the two actually belong together.
    pub fn from_parts(public: PublicKeyPackage, fvk_bytes: &[u8; 96]) -> Result<Self, VaultError> {
        let fvk = FullViewingKey::from_bytes(fvk_bytes).ok_or(VaultError::ViewingKeyMismatch)?;
        let ak = group_ak(&public)?;
        if fvk.to_bytes()[..32] != ak.to_bytes() {
            return Err(VaultError::ViewingKeyMismatch);
        }
        Ok(Self { public, fvk })
    }

    pub fn public_key_package(&self) -> &PublicKeyPackage {
        &self.public
    }

    pub fn fvk(&self) -> &FullViewingKey {
        &self.fvk
    }

    /// Unified viewing key with only the Orchard-protocol component, which
    /// covers Ironwood notes.
    pub fn ufvk(&self) -> UnifiedFullViewingKey {
        UnifiedFullViewingKey::from_orchard_fvk(self.fvk.clone())
            .expect("an Orchard-only UFVK is always valid")
    }

    /// Diversified receiving address `j`. Distinct indices give addresses that
    /// can't be linked to each other without the viewing key.
    pub fn address(&self, j: u32) -> UnifiedAddress {
        self.ufvk()
            .address(j.into(), UnifiedAddressRequest::ORCHARD)
            .expect("every Orchard diversifier index yields a valid address")
    }

    pub fn encode_address(&self, params: &impl Parameters, j: u32) -> String {
        self.address(j).encode(params)
    }
}

fn group_ak(public: &PublicKeyPackage) -> Result<SpendValidatingKey, VaultError> {
    let bytes = public
        .verifying_key()
        .serialize()
        .map_err(|_| VaultError::InvalidGroupKey)?;
    SpendValidatingKey::from_bytes(&bytes).ok_or(VaultError::InvalidGroupKey)
}

/// What a signer is being asked to authorize, recomputed from the PCZT.
#[derive(Clone, Debug)]
pub struct SigningRequest {
    pub sighash: [u8; 32],
    /// (Ironwood action index, randomizer alpha) for each spend needing a signature.
    pub spends: Vec<(usize, [u8; 32])>,
}

impl SigningRequest {
    pub fn from_pczt(pczt: &Pczt) -> Result<Self, VaultError> {
        let signer = Signer::new(pczt.clone()).map_err(|e| VaultError::Pczt(format!("{e:?}")))?;
        let sighash = signer.shielded_sighash();

        // Read each unsigned spend's randomizer. The closure only looks; it
        // leaves the bundle as it found it.
        let mut spends = Vec::new();
        low_level_signer::Signer::new(pczt.clone())
            .sign_ironwood_with(|_, bundle, _| -> Result<(), VaultError> {
                for (i, action) in bundle.actions().iter().enumerate() {
                    let spend = action.spend();
                    if spend.spend_auth_sig().is_some() {
                        // Dummy spends are signed by the IO finalizer already.
                        continue;
                    }
                    let alpha = spend.alpha().as_ref().ok_or(VaultError::MissingRandomizer(i))?;
                    spends.push((i, alpha.to_repr()));
                }
                Ok(())
            })?;
        Ok(Self { sighash, spends })
    }
}

impl From<low_level_signer::OrchardParseError> for VaultError {
    fn from(e: low_level_signer::OrchardParseError) -> Self {
        VaultError::Pczt(format!("{e:?}"))
    }
}

/// A signer's round one state: one nonce per spend, tied to the exact
/// transaction the signer was shown.
///
/// Ironwood fixes each spend's randomizer inside the PCZT, so we can't use
/// FROST's commitment-derived randomizer. Instead a signer only commits after
/// receiving the PCZT and refuses to sign any other transaction with these
/// nonces. That way the randomizers are settled before any nonce is revealed,
/// which is the property the commitment-derived randomizer exists to provide.
pub struct Round1 {
    sighash: [u8; 32],
    nonces: Vec<SigningNonces>,
    pub commitments: Vec<SigningCommitments>,
}

/// Round one, run by each participant once they've reviewed the PCZT.
pub fn commit(
    pczt: &Pczt,
    key_package: &KeyPackage,
    mut rng: impl RngCore + CryptoRng,
) -> Result<Round1, VaultError> {
    let request = SigningRequest::from_pczt(pczt)?;
    let (nonces, commitments) = (0..request.spends.len())
        .map(|_| frost::round1::commit(key_package.signing_share(), &mut rng))
        .unzip();
    Ok(Round1 { sighash: request.sighash, nonces, commitments })
}

/// Round two, run by each participant. `commitments` holds every chosen
/// signer's round one commitments, including this participant's.
/// Consumes the round one state so nonces can never be used twice.
pub fn sign(
    pczt: &Pczt,
    key_package: &KeyPackage,
    round1: Round1,
    commitments: &BTreeMap<Identifier, Vec<SigningCommitments>>,
) -> Result<Vec<SignatureShare>, VaultError> {
    let request = SigningRequest::from_pczt(pczt)?;
    if request.sighash != round1.sighash {
        return Err(VaultError::TransactionChanged);
    }
    check_commitment_counts(commitments, request.spends.len())?;
    let nonces = round1.nonces;
    request
        .spends
        .iter()
        .enumerate()
        .map(|(k, (_, alpha))| {
            let package = signing_package(commitments, k, &request.sighash);
            let randomizer = frost::rerandomized::Randomizer::deserialize(alpha)?;
            // See `Round1` for why the fixed randomizer is safe here.
            #[allow(deprecated)]
            let share = frost_rerandomized::sign(&package, &nonces[k], key_package, randomizer)?;
            Ok(share)
        })
        .collect()
}

/// Aggregation, run by the coordinator. Returns the PCZT with every vault
/// spend authorized.
pub fn aggregate(
    pczt: Pczt,
    vault: &Vault,
    commitments: &BTreeMap<Identifier, Vec<SigningCommitments>>,
    shares: &BTreeMap<Identifier, Vec<SignatureShare>>,
) -> Result<Pczt, VaultError> {
    let request = SigningRequest::from_pczt(&pczt)?;
    check_commitment_counts(commitments, request.spends.len())?;
    let mut signer = Signer::new(pczt).map_err(|e| VaultError::Pczt(format!("{e:?}")))?;

    for (k, (index, alpha)) in request.spends.iter().enumerate() {
        let package = signing_package(commitments, k, &request.sighash);
        let randomizer = frost::rerandomized::Randomizer::deserialize(alpha)?;
        let params = frost::rerandomized::RandomizedParams::from_randomizer(
            vault.public.verifying_key(),
            randomizer,
        );
        let spend_shares = shares
            .iter()
            .map(|(id, s)| {
                s.get(k)
                    .copied()
                    .map(|share| (*id, share))
                    .ok_or(VaultError::CommitmentCount { expected: request.spends.len(), got: s.len() })
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let sig = frost::rerandomized::aggregate(&package, &spend_shares, &vault.public, &params)?;
        let sig_bytes: [u8; 64] = sig
            .serialize()
            .map_err(VaultError::Frost)?
            .try_into()
            .expect("RedPallas signatures are 64 bytes");
        signer
            .apply_ironwood_signature(*index, redpallas::Signature::<SpendAuth>::from(sig_bytes))
            .map_err(|e| VaultError::Pczt(format!("{e:?}")))?;
    }
    Ok(signer.finish())
}

fn signing_package(
    commitments: &BTreeMap<Identifier, Vec<SigningCommitments>>,
    spend: usize,
    sighash: &[u8; 32],
) -> SigningPackage {
    let for_spend = commitments.iter().map(|(id, c)| (*id, c[spend])).collect();
    SigningPackage::new(for_spend, sighash)
}

fn check_commitment_counts(
    commitments: &BTreeMap<Identifier, Vec<SigningCommitments>>,
    expected: usize,
) -> Result<(), VaultError> {
    for c in commitments.values() {
        if c.len() != expected {
            return Err(VaultError::CommitmentCount { expected, got: c.len() });
        }
    }
    Ok(())
}
