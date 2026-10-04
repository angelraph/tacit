use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tacit_coordinator::{router, AppState};
use tacit_core::envelope::SeatKey;
use tower::ServiceExt;

async fn call(app: &axum::Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let req = Request::builder().method(method).uri(uri).header("content-type", "application/json");
    let req = req.body(body.map_or(Body::empty(), |b| Body::from(b.to_string()))).unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn terms(bond: u64) -> Value {
    json!({
        "network": "regtest",
        "vault_ufvk": "uviewregtest1example",
        "seller": "uregtest1seller",
        "item": "lot 7",
        "created_height": 10,
        "reserve": 10_000_000u64,
        "bond": bond,
        "bids_until_height": 100,
        "close_round": 32_633_680u64,
        "rule": "second_price"
    })
}

#[tokio::test]
async fn publishes_and_lists_auctions_with_server_computed_ids() {
    let app = router(AppState::in_memory().unwrap());
    let (status, rec) = call(&app, "POST", "/v1/auctions", Some(terms(100_000_000))).await;
    assert_eq!(status, StatusCode::OK);
    let id = rec["id"].as_str().unwrap().to_string();
    assert_eq!(id.len(), 64);

    let (_, got) = call(&app, "GET", &format!("/v1/auctions/{id}"), None).await;
    assert_eq!(got["terms"]["item"], "lot 7");
    let (_, list) = call(&app, "GET", "/v1/auctions", None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn rejects_invalid_terms() {
    let app = router(AppState::in_memory().unwrap());
    // 1.2 ZEC isn't a canonical denomination.
    let (status, err) = call(&app, "POST", "/v1/auctions", Some(terms(120_000_000))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(err["error"].as_str().unwrap().contains("canonical"));
}

#[tokio::test]
async fn relays_sealed_messages_only_between_seats() {
    let app = router(AppState::in_memory().unwrap());
    let (alice, bob, carol, eve) = (SeatKey::generate(), SeatKey::generate(), SeatKey::generate(), SeatKey::generate());
    let seats: Vec<String> = [&alice, &bob, &carol].iter().map(|k| hex::encode(k.public())).collect();

    let (status, room) = call(&app, "POST", "/v1/rooms", Some(json!({ "seats": seats }))).await;
    assert_eq!(status, StatusCode::OK);
    let room_id = room["id"].as_str().unwrap().to_string();
    let ctx = format!("{room_id}/dkg/2");

    let env = alice.seal(&bob.public(), ctx.as_bytes(), b"a secret share for bob");
    let msg = json!({ "from": seats[0], "to": seats[1], "round": 2, "body": hex::encode(&env) });
    let (status, _) = call(&app, "POST", &format!("/v1/rooms/{room_id}/messages"), Some(msg)).await;
    assert_eq!(status, StatusCode::OK);

    let (_, inbox) = call(&app, "GET", &format!("/v1/rooms/{room_id}/messages?to={}", seats[1]), None).await;
    let inbox = inbox.as_array().unwrap();
    assert_eq!(inbox.len(), 1);
    let body = hex::decode(inbox[0]["body"].as_str().unwrap()).unwrap();
    assert_eq!(bob.open(&alice.public(), ctx.as_bytes(), &body).unwrap(), b"a secret share for bob");

    // Carol's inbox is empty, and reading past the last seq returns nothing new.
    let (_, empty) = call(&app, "GET", &format!("/v1/rooms/{room_id}/messages?to={}", seats[2]), None).await;
    assert!(empty.as_array().unwrap().is_empty());
    let seq = inbox[0]["seq"].as_i64().unwrap();
    let (_, none) = call(&app, "GET", &format!("/v1/rooms/{room_id}/messages?to={}&after={seq}", seats[1]), None).await;
    assert!(none.as_array().unwrap().is_empty());

    // Someone outside the room can't post into it.
    let eve_hex = hex::encode(eve.public());
    let forged = json!({ "from": eve_hex, "to": seats[1], "round": 2, "body": "00" });
    let (status, _) = call(&app, "POST", &format!("/v1/rooms/{room_id}/messages"), Some(forged)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
