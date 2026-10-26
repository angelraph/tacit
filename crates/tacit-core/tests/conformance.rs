//! Replays real, settled auctions from `test-vectors/` offline.
//!
//! Each vector holds an auction's published terms, every deposit its vault
//! received (memo included) as read through the published viewing key, the
//! drand beacon for its closing round, and what the settlement transaction
//! actually paid on chain. The rules in `auction::settle` must reproduce that
//! payout exactly, with no network access. Vectors are made with
//! `tacit-dev vector <gateway> <auction id>`.

use std::path::PathBuf;

use serde_json::Value;
use tacit_core::auction::{self, Deposit, Terms, Where};
use tacit_core::beacon::Beacon;

fn vectors() -> Vec<(String, Value)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-vectors");
    let mut out: Vec<(String, Value)> = std::fs::read_dir(&dir)
        .expect("test-vectors directory")
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .map(|p| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            (name, serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap())
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn sorted(mut v: Vec<(String, u64)>) -> Vec<(String, u64)> {
    v.sort();
    v
}

#[test]
fn settled_auctions_replay_exactly() {
    let vectors = vectors();
    assert!(!vectors.is_empty(), "no test vectors found");
    for (name, v) in vectors {
        let terms: Terms = serde_json::from_value(v["terms"].clone()).unwrap();
        assert_eq!(hex::encode(terms.id()), v["auction_id"].as_str().unwrap(), "{name}: auction id");

        let round = v["beacon"]["round"].as_u64().unwrap();
        let beacon = Beacon::verify(round, &hex::decode(v["beacon"]["signature"].as_str().unwrap()).unwrap())
            .unwrap_or_else(|e| panic!("{name}: beacon doesn't verify against the quicknet key: {e}"));

        let deposits: Vec<Deposit> = v["deposits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| Deposit {
                id: hex::decode(d["id"].as_str().unwrap()).unwrap(),
                value: d["value"].as_u64().unwrap(),
                mined_height: d["mined_height"].as_u64().unwrap() as u32,
                memo: hex::decode(d["memo"].as_str().unwrap()).unwrap(),
            })
            .collect();

        let outcome = auction::settle(&terms, &deposits, &beacon).unwrap();
        assert_eq!(outcome.winner.as_ref().map(hex::encode), v["expected"]["winner"].as_str().map(String::from), "{name}: winner");
        assert_eq!(outcome.price, v["expected"]["price"].as_u64(), "{name}: price");
        assert_eq!(Some(outcome.fee), v["expected"]["fee"].as_u64(), "{name}: fee");

        let seller = v["seller_receiver"].as_str().unwrap().to_string();
        let computed = sorted(
            outcome
                .payouts
                .iter()
                .map(|p| match &p.to {
                    Where::Seller => (seller.clone(), p.amount),
                    Where::Receiver(raw) => (hex::encode(raw), p.amount),
                })
                .collect(),
        );
        let on_chain = sorted(
            v["on_chain"]["payments"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| (p["to"].as_str().unwrap().to_string(), p["amount"].as_u64().unwrap()))
                .collect(),
        );
        assert_eq!(computed, on_chain, "{name}: the rules must reproduce what the vault paid on chain");
    }
}
