//! Times the same Ironwood proof natively, for comparison with the browser.
use orchard::bundle::BundleVersion;
use orchard::circuit::ProvingKey;
use std::time::Instant;

fn main() {
    let t = Instant::now();
    let pk = ProvingKey::build(BundleVersion::ironwood_v3().circuit_version());
    println!("proving key: {:.1} s", t.elapsed().as_secs_f64());
    for outputs in [2, 2, 5] {
        let t = Instant::now();
        tacit_wasm::prove_bundle(&pk, outputs).unwrap();
        println!("proof, {outputs} output(s): {:.1} s", t.elapsed().as_secs_f64());
    }
}
