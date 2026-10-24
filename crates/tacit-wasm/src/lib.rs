//! Tacit in the browser.
//!
//! [`app`] is what the web app uses: a wallet, sealed bids and auction audits.
//! The prover benchmark below is what we measured browser proving with.

pub mod app;
pub mod seat;

use orchard::builder::{Builder, BundleType};
use orchard::bundle::{Authorized, Bundle, BundleVersion};
use orchard::circuit::ProvingKey;
use orchard::keys::{FullViewingKey, Scope, SpendingKey};
use orchard::tree::Anchor;
use orchard::value::NoteValue;
use rand::rngs::OsRng;
use wasm_bindgen::prelude::*;

#[cfg(feature = "threads")]
pub use wasm_bindgen_rayon::init_thread_pool;

/// Holds the Ironwood proving key. Building it is a one-off cost per page
/// load, so the app builds it once in a worker and keeps it.
#[wasm_bindgen]
pub struct IronwoodProver {
    pk: ProvingKey,
}

#[wasm_bindgen]
impl IronwoodProver {
    #[wasm_bindgen(constructor)]
    pub fn new() -> IronwoodProver {
        IronwoodProver { pk: ProvingKey::build(BundleVersion::ironwood_v3().circuit_version()) }
    }

    /// Builds and proves an Ironwood bundle with `outputs` real outputs
    /// (padded to at least two actions, like every wallet transaction).
    /// Returns the proof size in bytes.
    pub fn prove(&self, outputs: u32) -> Result<usize, JsError> {
        let bundle = prove_bundle(&self.pk, outputs).map_err(|e| JsError::new(&e))?;
        Ok(bundle.authorization().proof().as_ref().len())
    }
}

impl Default for IronwoodProver {
    fn default() -> Self {
        Self::new()
    }
}

/// Builds a real Ironwood bundle paying `outputs` fresh addresses and creates
/// its Halo2 proof with the post NU6.3 circuit.
///
/// The bundle is then signed (over a fixed sighash, since there's no enclosing
/// transaction), so the result is a complete, authorized bundle.
pub fn prove_bundle(pk: &ProvingKey, outputs: u32) -> Result<Bundle<Authorized, i64>, String> {
    let version = BundleVersion::ironwood_v3();
    let mut builder = Builder::new(BundleType::DEFAULT, version, version.default_flags(), Anchor::empty_tree())
        .map_err(|e| format!("{e:?}"))?;
    let sk = loop {
        let mut b = [0u8; 32];
        rand::RngCore::fill_bytes(&mut OsRng, &mut b);
        if let Some(sk) = Option::<SpendingKey>::from(SpendingKey::from_bytes(b)) {
            break sk;
        }
    };
    let fvk = FullViewingKey::from(&sk);
    for i in 0..outputs {
        builder
            .add_output(None, fvk.address_at(i, Scope::External), NoteValue::from_raw(1_000 + i as u64), [0; 512])
            .map_err(|e| format!("{e:?}"))?;
    }
    let (bundle, _) = builder
        .build::<i64>(OsRng)
        .map_err(|e| format!("{e:?}"))?
        .ok_or("builder produced no bundle")?;
    bundle
        .create_proof(pk, OsRng)
        .map_err(|e| format!("{e:?}"))?
        .apply_signatures(OsRng, [0u8; 32], &[])
        .map_err(|e| format!("{e:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchard::circuit::VerifyingKey;

    #[test]
    fn the_benchmark_proof_is_valid() {
        let version = BundleVersion::ironwood_v3();
        let pk = ProvingKey::build(version.circuit_version());
        let vk = VerifyingKey::build(version.circuit_version());
        let bundle = prove_bundle(&pk, 2).unwrap();
        let instances: Vec<_> = bundle
            .actions()
            .iter()
            .map(|a| a.to_instance(*bundle.flags(), *bundle.anchor()))
            .collect();
        bundle
            .authorization()
            .proof()
            .verify(&vk, &instances)
            .expect("the proof we time must be a valid Ironwood proof");
    }
}
