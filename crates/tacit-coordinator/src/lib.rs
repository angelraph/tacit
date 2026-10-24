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
use tacit_core::escrow::EscrowTerms;

pub mod chain;

/// Largest message body accepted (a PCZT for a big settlement fits easily).
pub const MAX_BODY: usize = 1 << 20;

#[derive(Clone)]
pub struct AppState {
    db: Arc<Mutex<Connection>>,
    chain: Option<chain::Chain>,
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
                 purpose TEXT NOT NULL DEFAULT '',
                 created INTEGER NOT NULL DEFAULT (unixepoch())
             );
             CREATE TABLE IF NOT EXISTS room_seats (
                 room TEXT NOT NULL REFERENCES rooms(id),
                 seat TEXT NOT NULL,
                 PRIMARY KEY (seat, room)
             );
             CREATE TABLE IF NOT EXISTS agents (
                 seat TEXT PRIMARY KEY,
                 role TEXT NOT NULL,
                 name TEXT NOT NULL,
                 seen INTEGER NOT NULL DEFAULT (unixepoch())
             );
             CREATE TABLE IF NOT EXISTS escrows (
                 id TEXT PRIMARY KEY,
                 terms TEXT NOT NULL,
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
        // Databases from before rooms had a purpose.
        let has_purpose = conn
            .prepare("SELECT purpose FROM rooms LIMIT 0")
            .is_ok();
        if !has_purpose {
            conn.execute_batch("ALTER TABLE rooms ADD COLUMN purpose TEXT NOT NULL DEFAULT ''")?;
        }
        Ok(AppState { db: Arc::new(Mutex::new(conn)), chain: None })
    }

    /// Also relays chain data from the lightwalletd-compatible gRPC server at
    /// `url`, so browsers can sync and broadcast through this coordinator.
    pub fn with_chain(mut self, url: &str) -> Result<Self, String> {
        self.chain = Some(chain::Chain::new(url)?);
        Ok(self)
    }

    pub fn in_memory() -> rusqlite::Result<Self> {
        Self::open(":memory:")
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/auctions", post(publish_auction).get(list_auctions))
        .route("/v1/auctions/{id}", get(get_auction))
        .route("/v1/rooms", post(open_room).get(rooms_for_seat))
        .route("/v1/rooms/{id}", get(get_room))
        .route("/v1/rooms/{id}/messages", post(post_message).get(read_messages))
        .route("/v1/agents", post(announce_agent).get(list_agents))
        .route("/v1/escrows", post(publish_escrow).get(list_escrows))
        .route("/v1/escrows/{id}", get(get_escrow))
        .route("/v1/chain/info", get(chain::info))
        .route("/v1/chain/tree/{height}", get(chain::tree))
        .route("/v1/chain/blocks", get(chain::blocks))
        .route("/v1/chain/tx", post(chain::send_tx))
        .route("/v1/chain/tx/{txid}", get(chain::get_tx))
        .route("/v1/chain/utxos/{addr}", get(chain::utxos))
        .layer(DefaultBodyLimit::max(MAX_BODY * 2 + 4096))
        // Browsers on any origin may use it: everything here is public data
        // or already sealed.
        .layer(tower_http::cors::CorsLayer::permissive())
        .with_state(state)
}

/// Serves a coordinator on the current tokio runtime in the background and
/// returns its base URL. Pass port 0 to get a free one.
pub async fn spawn(addr: &str, state: AppState) -> std::io::Result<String> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    tokio::spawn(async move {
        axum::serve(listener, router(state)).await.expect("coordinator stopped");
    });
    Ok(format!("http://{local}"))
}

pub struct ApiError(pub StatusCode, pub String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}

pub(crate) fn bad(msg: impl Into<String>) -> ApiError {
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
    /// What the room is for, so seats that run unattended know what to do in
    /// it: `dkg`, `settle:<auction id>` or `escrow:<escrow id>:<pay>`.
    /// Public, like everything about a room except its messages.
    #[serde(default)]
    pub purpose: String,
    #[serde(default)]
    pub created: i64,
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
    if room.purpose.len() > 200 {
        return Err(bad("purpose is too long"));
    }
    let mut id = [0u8; 16];
    OsRng.fill_bytes(&mut id);
    let id = hex::encode(id);
    let db = s.db.lock().unwrap();
    db.execute(
        "INSERT INTO rooms (id, seats, purpose) VALUES (?1, ?2, ?3)",
        params![id, serde_json::to_string(&room.seats).unwrap(), room.purpose],
    )?;
    for seat in &room.seats {
        db.execute("INSERT OR IGNORE INTO room_seats (room, seat) VALUES (?1, ?2)", params![id, seat])?;
    }
    let created: i64 = db.query_row("SELECT created FROM rooms WHERE id = ?1", [&id], |r| r.get(0))?;
    Ok(Json(Room { id, seats: room.seats, purpose: room.purpose, created }))
}

#[derive(Deserialize)]
pub struct SeatQuery {
    seat: String,
    #[serde(default)]
    since: i64,
}

/// Rooms a seat sits in, oldest first. Agents poll this to find work.
async fn rooms_for_seat(State(s): State<AppState>, Query(q): Query<SeatQuery>) -> Result<Json<Vec<Room>>, ApiError> {
    check_key(&q.seat)?;
    let db = s.db.lock().unwrap();
    let mut stmt = db.prepare(
        "SELECT r.id, r.seats, r.purpose, r.created FROM rooms r JOIN room_seats x ON x.room = r.id
         WHERE x.seat = ?1 AND r.created >= ?2 ORDER BY r.created, r.id LIMIT 500",
    )?;
    let rows = stmt.query_map(params![q.seat, q.since], |r| {
        Ok(Room {
            id: r.get(0)?,
            seats: serde_json::from_str(&r.get::<_, String>(1)?).unwrap_or_default(),
            purpose: r.get(2)?,
            created: r.get(3)?,
        })
    })?;
    Ok(Json(rows.collect::<Result<_, _>>()?))
}

/// An always-online seat (an operator or an arbiter) that sellers and escrow
/// parties can pick. Agents re-announce themselves while they run.
#[derive(Serialize, Deserialize)]
pub struct Agent {
    pub seat: String,
    /// `operator` or `arbiter`.
    pub role: String,
    pub name: String,
    #[serde(default)]
    pub seen: i64,
}

async fn announce_agent(State(s): State<AppState>, Json(a): Json<Agent>) -> Result<Json<Agent>, ApiError> {
    check_key(&a.seat)?;
    if a.role != "operator" && a.role != "arbiter" {
        return Err(bad("role must be operator or arbiter"));
    }
    if a.name.is_empty() || a.name.len() > 64 {
        return Err(bad("name must be 1 to 64 bytes"));
    }
    s.db.lock().unwrap().execute(
        "INSERT INTO agents (seat, role, name, seen) VALUES (?1, ?2, ?3, unixepoch())
         ON CONFLICT (seat) DO UPDATE SET role = ?2, name = ?3, seen = unixepoch()",
        params![a.seat, a.role, a.name],
    )?;
    Ok(Json(a))
}

/// Agents seen in the last ten minutes.
async fn list_agents(State(s): State<AppState>) -> Result<Json<Vec<Agent>>, ApiError> {
    let db = s.db.lock().unwrap();
    let mut stmt =
        db.prepare("SELECT seat, role, name, seen FROM agents WHERE seen > unixepoch() - 600 ORDER BY role, seen DESC")?;
    let rows = stmt.query_map([], |r| Ok(Agent { seat: r.get(0)?, role: r.get(1)?, name: r.get(2)?, seen: r.get(3)? }))?;
    Ok(Json(rows.collect::<Result<_, _>>()?))
}

#[derive(Serialize, Deserialize)]
pub struct EscrowRecord {
    pub id: String,
    pub terms: EscrowTerms,
}

async fn publish_escrow(State(s): State<AppState>, Json(terms): Json<EscrowTerms>) -> Result<Json<EscrowRecord>, ApiError> {
    terms.validate().map_err(|e| bad(e.to_string()))?;
    let id = hex::encode(terms.id());
    let json = serde_json::to_string(&terms).expect("terms serialize");
    s.db.lock().unwrap().execute(
        "INSERT INTO escrows (id, terms) VALUES (?1, ?2) ON CONFLICT (id) DO NOTHING",
        params![id, json],
    )?;
    Ok(Json(EscrowRecord { id, terms }))
}

async fn list_escrows(State(s): State<AppState>) -> Result<Json<Vec<EscrowRecord>>, ApiError> {
    let db = s.db.lock().unwrap();
    let mut stmt = db.prepare("SELECT id, terms FROM escrows ORDER BY created DESC LIMIT 500")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = Vec::new();
    for row in rows {
        let (id, terms) = row?;
        out.push(EscrowRecord { id, terms: serde_json::from_str(&terms).expect("stored terms are valid") });
    }
    Ok(Json(out))
}

async fn get_escrow(State(s): State<AppState>, Path(id): Path<String>) -> Result<Json<EscrowRecord>, ApiError> {
    let terms: Option<String> = s
        .db
        .lock()
        .unwrap()
        .query_row("SELECT terms FROM escrows WHERE id = ?1", [&id], |r| r.get(0))
        .optional()?;
    let terms = terms.ok_or_else(|| not_found("escrow"))?;
    Ok(Json(EscrowRecord { id, terms: serde_json::from_str(&terms).expect("stored terms are valid") }))
}

fn room_seats(db: &Connection, id: &str) -> Result<Vec<String>, ApiError> {
    let seats: Option<String> = db
        .query_row("SELECT seats FROM rooms WHERE id = ?1", [id], |r| r.get(0))
        .optional()?;
    let seats = seats.ok_or_else(|| not_found("room"))?;
    Ok(serde_json::from_str(&seats).expect("stored seats are valid"))
}

async fn get_room(State(s): State<AppState>, Path(id): Path<String>) -> Result<Json<Room>, ApiError> {
    let db = s.db.lock().unwrap();
    let seats = room_seats(&db, &id)?;
    let (purpose, created) =
        db.query_row("SELECT purpose, created FROM rooms WHERE id = ?1", [&id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(Json(Room { id, seats, purpose, created }))
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
