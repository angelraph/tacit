//! Live check against the real drand quicknet network: seal a bid-sized
//! payload to a round a few seconds out, show it can't be opened early,
//! then fetch and verify the beacon and open it.

use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tacit_core::beacon::Beacon;
use tacit_core::seal::{self, QUICKNET_CHAIN_HASH};

const RELAYS: [&str; 2] = ["https://api.drand.sh", "https://drand.cloudflare.com"];
/// Largest memo Zcash allows.
const MEMO_LEN: usize = 512;

fn fetch(round: u64) -> Option<Beacon> {
    for relay in RELAYS {
        let url = format!("{relay}/{QUICKNET_CHAIN_HASH}/public/{round}");
        let Ok(mut res) = ureq::get(&url).call() else { continue };
        let Ok(text) = res.body_mut().read_to_string() else { continue };
        let Ok(body) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
        let sig = hex::decode(body["signature"].as_str()?).ok()?;
        match Beacon::verify(round, &sig) {
            Ok(b) => return Some(b),
            Err(e) => eprintln!("{relay} returned a bad beacon: {e}"),
        }
    }
    None
}

fn main() {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let round = seal::round_at(now + 15);

    // bid in zatoshi (8) + raw Orchard receiver for refunds (43) + auction id (32)
    let mut payload = Vec::new();
    payload.extend_from_slice(&1_250_000_000u64.to_be_bytes());
    payload.extend_from_slice(&[0xab; 43]);
    payload.extend_from_slice(&[0xcd; 32]);

    let sealed = seal::seal(&payload, round).expect("seal");
    println!("sealed {} byte payload into {} bytes ({} left in a {MEMO_LEN} byte memo)",
        payload.len(), sealed.len(), MEMO_LEN - sealed.len());
    println!("target round {round}, opens at unix {}", seal::round_time(round));

    match fetch(round) {
        None => println!("before the round: no beacon exists yet, box stays shut"),
        Some(_) => panic!("beacon already published, clock is off"),
    }

    let beacon = loop {
        if let Some(b) = fetch(round) {
            break b;
        }
        std::thread::sleep(Duration::from_secs(1));
    };
    println!("beacon for round {} verified against the quicknet key", beacon.round());
    let opened = seal::open(&sealed, &beacon).expect("open");
    assert_eq!(opened, payload);
    println!("opened: bid = {} zatoshi, payload intact", u64::from_be_bytes(opened[..8].try_into().unwrap()));
}
