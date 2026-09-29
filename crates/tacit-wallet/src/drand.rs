//! Fetching drand quicknet beacons. Relays are untrusted: every beacon is
//! checked against the quicknet group key before it's returned.

use std::time::Duration;

use tacit_core::beacon::Beacon;
use tacit_core::seal::QUICKNET_CHAIN_HASH;

pub const RELAYS: [&str; 3] = [
    "https://api.drand.sh",
    "https://drand.cloudflare.com",
    "https://api2.drand.sh",
];

/// The verified beacon for `round`, or `None` if it isn't published yet.
pub fn fetch(round: u64) -> Option<Beacon> {
    for relay in RELAYS {
        let url = format!("{relay}/{QUICKNET_CHAIN_HASH}/public/{round}");
        let Ok(mut res) = ureq::get(&url).call() else { continue };
        let Ok(text) = res.body_mut().read_to_string() else { continue };
        let Ok(body) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
        let Some(sig) = body["signature"].as_str().and_then(|s| hex::decode(s).ok()) else { continue };
        if let Ok(beacon) = Beacon::verify(round, &sig) {
            return Some(beacon);
        }
    }
    None
}

/// Blocks until `round` is published and verified.
pub fn wait_for(round: u64) -> Beacon {
    loop {
        if let Some(b) = fetch(round) {
            return b;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}
