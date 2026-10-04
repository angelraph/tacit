//! Tacit's coordinator: an auction registry and a mailbox for vault seats.
//!
//! It holds no keys and can't read what seats send each other. Every message
//! between seats is a sealed envelope (see `tacit_core::envelope`), so the
//! worst a misbehaving coordinator can do is drop or delay messages. Auction
//! terms are public by design; the coordinator recomputes each auction's id
//! itself rather than trusting the one a client claims.

use std::sync::{Arc, Mutex};

use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rand_core::{OsRng, RngCore};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tacit_core::auction::Terms;

/// Largest message body accepted (a PCZT for a big settlement fits easily).
pub const MAX_BODY: usize = 1 << 20;

#[derive(Clone)]
pub struct AppState {
    db: Arc<Mutex<Connection>>,
}

impl AppState {
    pub fn open(path: &str) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS auctions (
                 id TEXT PRIMARY KEY,
                 terms TEXT NOT NULL,
                 created INTEGER NOT NULL DEFAULT (unixepoch())
             );
             CREATE TABLE IF NOT EXISTS rooms (
                 id TEXT PRIMARY KEY,
                 seats TEXT NOT NULL,
                 created INTEGER NOT NULL DEFAULT (unixepoch())
             );
             CREATE TABLE IF NOT EXISTS messages (
                 seq INTEGER PRIMARY KEY AUTOINCREMENT,
                 room TEXT NOT NULL REFERENCES rooms(id),
                 sender TEXT NOT NULL,
                 recipient TEXT NOT NULL,
                 round INTEGER NOT NULL,
                 body BLOB NOT NULL,
                 created INTEGER NOT NULL DEFAULT (unixepoch())
             );
             CREATE INDEX IF NOT EXISTS messages_inbox ON messages (room, recipient, seq);",
        )?;
        Ok(AppState { db: Arc::new(Mutex::new(conn)) })
    }

    pub fn in_memory() -> rusqlite::Result<Self> {
        Self::open(":memory:")
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/auctions", post(publish_auction).get(list_auctions))
        .route("/v1/auctions/{id}", get(get_auction))
        .route("/v1/rooms", post(open_room))
        .route("/v1/rooms/{id}", get(get_room))
        .route("/v1/rooms/{id}/messages", post(post_message).get(read_messages))
        .layer(DefaultBodyLimit::max(MAX_BODY * 2 + 4096))
        .with_state(state)
}

pub struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}

fn not_found(what: &str) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, format!("no such {what}"))
}

impl From<rusqlite::Error> for ApiError {
    fn from(e: rusqlite::Error) -> Self {
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    }
}

#[derive(Serialize, Deserialize)]
pub struct AuctionRecord {
    pub id: String,
    pub terms: Terms,
}

async fn publish_auction(State(s): State<AppState>, Json(terms): Json<Terms>) -> Result<Json<AuctionRecord>, ApiError> {
    terms.validate().map_err(|e| bad(e.to_string()))?;
    let id = hex::encode(terms.id());
    let json = serde_json::to_string(&terms).expect("terms serialize");
    s.db.lock().unwrap().execute(
        "INSERT INTO auctions (id, terms) VALUES (?1, ?2) ON CONFLICT (id) DO NOTHING",
        params![id, json],
    )?;
    Ok(Json(AuctionRecord { id, terms }))
}

async fn list_auctions(State(s): State<AppState>) -> Result<Json<Vec<AuctionRecord>>, ApiError> {
    let db = s.db.lock().unwrap();
    let mut stmt = db.prepare("SELECT id, terms FROM auctions ORDER BY created DESC LIMIT 200")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = Vec::new();
    for row in rows {
        let (id, terms) = row?;
        out.push(AuctionRecord { id, terms: serde_json::from_str(&terms).expect("stored terms are valid") });
    }
    Ok(Json(out))
}

async fn get_auction(State(s): State<AppState>, Path(id): Path<String>) -> Result<Json<AuctionRecord>, ApiError> {
    let terms: Option<String> = s
        .db
        .lock()
        .unwrap()
        .query_row("SELECT terms FROM auctions WHERE id = ?1", [&id], |r| r.get(0))
        .optional()?;
    let terms = terms.ok_or_else(|| not_found("auction"))?;
    Ok(Json(AuctionRecord { id, terms: serde_json::from_str(&terms).expect("stored terms are valid") }))
}

#[derive(Serialize, Deserialize)]
pub struct Room {
    #[serde(default)]
    pub id: String,
    /// Hex X25519 public keys of the seats allowed to talk in this room.
    pub seats: Vec<String>,
}

fn check_key(k: &str) -> Result<(), ApiError> {
    match hex::decode(k) {
        Ok(b) if b.len() == 32 => Ok(()),
        _ => Err(bad(format!("{k} is not a 32 byte hex public key"))),
    }
}

async fn open_room(State(s): State<AppState>, Json(room): Json<Room>) -> Result<Json<Room>, ApiError> {
    if room.seats.len() < 2 || room.seats.len() > 16 {
        return Err(bad("a room needs between 2 and 16 seats"));
    }
    for k in &room.seats {
        check_key(k)?;
    }
    let mut id = [0u8; 16];
    OsRng.fill_bytes(&mut id);
    let id = hex::encode(id);
    s.db.lock().unwrap().execute(
        "INSERT INTO rooms (id, seats) VALUES (?1, ?2)",
        params![id, serde_json::to_string(&room.seats).unwrap()],
    )?;
    Ok(Json(Room { id, seats: room.seats }))
}

fn room_seats(db: &Connection, id: &str) -> Result<Vec<String>, ApiError> {
    let seats: Option<String> = db
        .query_row("SELECT seats FROM rooms WHERE id = ?1", [id], |r| r.get(0))
        .optional()?;
    let seats = seats.ok_or_else(|| not_found("room"))?;
    Ok(serde_json::from_str(&seats).expect("stored seats are valid"))
}

async fn get_room(State(s): State<AppState>, Path(id): Path<String>) -> Result<Json<Room>, ApiError> {
    let seats = room_seats(&s.db.lock().unwrap(), &id)?;
    Ok(Json(Room { id, seats }))
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Message {
    #[serde(default)]
    pub seq: i64,
    pub from: String,
    pub to: String,
    pub round: u8,
    /// Hex sealed envelope.
    pub body: String,
}

async fn post_message(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(m): Json<Message>,
) -> Result<Json<Message>, ApiError> {
    let body = hex::decode(&m.body).map_err(|_| bad("body is not hex"))?;
    if body.len() > MAX_BODY {
        return Err(bad("message too large"));
    }
    let db = s.db.lock().unwrap();
    let seats = room_seats(&db, &id)?;
    if !seats.contains(&m.from) || !seats.contains(&m.to) || m.from == m.to {
        return Err(bad("sender and recipient must be two different seats of this room"));
    }
    db.execute(
        "INSERT INTO messages (room, sender, recipient, round, body) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, m.from, m.to, m.round, body],
    )?;
    let seq = db.last_insert_rowid();
    Ok(Json(Message { seq, ..m }))
}

#[derive(Deserialize)]
pub struct Inbox {
    to: String,
    #[serde(default)]
    after: i64,
}

async fn read_messages(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<Inbox>,
) -> Result<Json<Vec<Message>>, ApiError> {
    let db = s.db.lock().unwrap();
    room_seats(&db, &id)?;
    let mut stmt = db.prepare(
        "SELECT seq, sender, recipient, round, body FROM messages
         WHERE room = ?1 AND recipient = ?2 AND seq > ?3 ORDER BY seq LIMIT 500",
    )?;
    let rows = stmt.query_map(params![id, q.to, q.after], |r| {
        Ok(Message {
            seq: r.get(0)?,
            from: r.get(1)?,
            to: r.get(2)?,
            round: r.get(3)?,
            body: hex::encode(r.get::<_, Vec<u8>>(4)?),
        })
    })?;
    Ok(Json(rows.collect::<Result<_, _>>()?))
}
