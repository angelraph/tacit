//! Three seats run key generation against a real coordinator over HTTP,
//! each on its own thread, and end up holding the same vault.

use std::time::Duration;

use tacit_core::envelope::SeatKey;
use tacit_vault::session::SeatPub;
use tacit_wallet::relay::{run_dkg, Relay, RelayError};

async fn coordinator() -> String {
    let state = tacit_coordinator::AppState::in_memory().unwrap();
    tacit_coordinator::spawn("127.0.0.1:0", state).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn three_seats_build_one_vault_over_http() {
    let url = coordinator().await;
    tokio::task::spawn_blocking(move || {
        let keys: Vec<[u8; 32]> = (0..3).map(|_| SeatKey::generate().to_bytes()).collect();
        let pubs: Vec<SeatPub> = keys.iter().map(|k| SeatKey::from_bytes(*k).public()).collect();
        let room = Relay::new(&url).open_room(&pubs).unwrap();
        let threads: Vec<_> = keys
            .into_iter()
            .map(|k| {
                let (url, room, pubs) = (url.clone(), room.clone(), pubs.clone());
                std::thread::spawn(move || {
                    run_dkg(&Relay::new(url), SeatKey::from_bytes(k), &room, pubs, Duration::from_secs(30))
                })
            })
            .collect();
        let shares: Vec<_> = threads.into_iter().map(|t| t.join().unwrap().unwrap()).collect();
        let fvk = shares[0].vault.fvk().to_bytes();
        assert!(shares.iter().all(|s| s.vault.fvk().to_bytes() == fvk));
        assert_eq!(shares.iter().map(|s| s.seat).collect::<Vec<_>>(), vec![1, 2, 3]);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_seat_times_out_instead_of_hanging() {
    let url = coordinator().await;
    tokio::task::spawn_blocking(move || {
        let keys: Vec<SeatKey> = (0..3).map(|_| SeatKey::generate()).collect();
        let pubs: Vec<SeatPub> = keys.iter().map(|k| k.public()).collect();
        let relay = Relay::new(&url);
        let room = relay.open_room(&pubs).unwrap();
        let key = keys.into_iter().next().unwrap();
        let res = run_dkg(&relay, key, &room, pubs, Duration::from_secs(1));
        assert!(matches!(res, Err(RelayError::Timeout)), "{:?}", res.err());
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stranger_cannot_take_a_seat() {
    let url = coordinator().await;
    tokio::task::spawn_blocking(move || {
        let pubs: Vec<SeatPub> = (0..3).map(|_| SeatKey::generate().public()).collect();
        let relay = Relay::new(&url);
        let room = relay.open_room(&pubs).unwrap();
        let stranger = SeatKey::generate();
        // A stranger isn't one of the seats, so the session won't even start.
        let res = run_dkg(&relay, stranger, &room, pubs, Duration::from_secs(1));
        assert!(res.is_err());
    })
    .await
    .unwrap();
}
