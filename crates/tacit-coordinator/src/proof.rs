//! Public proof: the coordinator audits every published auction itself and
//! keeps the reports, so anyone can see at a glance how every market ended.
//!
//! The audit is the same code a browser runs from "Run audit in this tab"
//! (`tacit_light::audit`), fed with the same public data: the published
//! terms, compact blocks, the deposit and settlement transactions, and the
//! drand beacon. Nothing here needs trusting: every report can be re-run in
//! any browser, and the page links to that.

use std::time::Duration;

use axum::extract::State;
use axum::Json;
use rusqlite::params;
use serde::Serialize;
use serde_json::Value;
use tacit_core::auction::Terms;
use tacit_core::beacon::Beacon;
use tacit_light::audit::{Audit, Report};

use crate::{chain, ApiError, AppState};

const QUICKNET: &str = "52db9ba70e0cc0f6eaf7803dd07447a1f5477735fd3f661792ba94600c84e971";
const EVERY: Duration = Duration::from_secs(120);

/// Audits every auction now and then, and keeps the latest report of each.
pub fn spawn_auditor(s: AppState) {
    tokio::spawn(async move {
        loop {
            let auctions: Vec<(String, String)> = {
                let db = s.db.lock().unwrap();
                // Settled-honestly reports are final; everything else is redone.
                let mut stmt = db
                    .prepare(
                        "SELECT a.id, a.terms FROM auctions a LEFT JOIN audits r ON r.id = a.id
                         WHERE r.verdict IS NULL OR r.verdict != 'honest'
                         ORDER BY a.created DESC LIMIT 100",
                    )
                    .expect("audit query");
                stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                    .map(|rows| rows.filter_map(Result::ok).collect())
                    .unwrap_or_default()
            };
            for (id, terms) in auctions {
                let Ok(terms) = serde_json::from_str::<Terms>(&terms) else { continue };
                match audit(&s, terms).await {
                    Ok(report) => {
                        let verdict = serde_json::to_value(&report.verdict).ok().and_then(|v| v.as_str().map(String::from));
                        let body = serde_json::to_string(&report).unwrap_or_default();
                        let _ = s.db.lock().unwrap().execute(
                            "INSERT INTO audits (id, verdict, report, updated) VALUES (?1, ?2, ?3, unixepoch())
                             ON CONFLICT (id) DO UPDATE SET verdict = ?2, report = ?3, updated = unixepoch()",
                            params![id, verdict.unwrap_or_default(), body],
                        );
                    }
                    Err(e) => eprintln!("proof: auditing {id}: {e}"),
                }
            }
            tokio::time::sleep(EVERY).await;
        }
    });
}

async fn audit(s: &AppState, terms: Terms) -> Result<Report, String> {
    let ch = s.chain.as_ref().ok_or("no chain gateway")?;
    let birthday = convert::<tacit_light::chain::Tree>(&ch.tree_state(terms.created_height as u64).await?)?;
    let close_round = terms.close_round;
    let mut audit = Audit::new(terms, &birthday).map_err(|e| e.to_string())?;
    let tip = ch.tip().await?;
    let mut h = audit.height() as u64;
    while h < tip {
        let to = tip.min(h + chain::MAX_BLOCKS);
        let blocks: Vec<tacit_light::chain::Block> = serde_json::from_str(&ch.blocks_json(h + 1, to).await?)
            .map_err(|e| e.to_string())?;
        audit.scanner().scan(&blocks).map_err(|e| e.to_string())?;
        h = audit.height() as u64;
    }
    for txid in audit.wanted() {
        let raw = convert::<tacit_light::chain::RawTx>(&ch.raw_tx(&txid).await?)?;
        audit.add_tx(&txid, &raw).map_err(|e| e.to_string())?;
    }
    let beacon = beacon(close_round).await;
    audit.report(beacon.as_ref()).map_err(|e| e.to_string())
}

fn convert<T: serde::de::DeserializeOwned>(v: &impl Serialize) -> Result<T, String> {
    serde_json::from_value(serde_json::to_value(v).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

/// The verified beacon for `round`, once drand has published it.
async fn beacon(round: u64) -> Option<Beacon> {
    tokio::task::spawn_blocking(move || {
        for relay in ["https://api.drand.sh", "https://drand.cloudflare.com", "https://api2.drand.sh"] {
            let Ok(mut res) = ureq::get(&format!("{relay}/{QUICKNET}/public/{round}")).call() else { continue };
            let Ok(text) = res.body_mut().read_to_string() else { continue };
            let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
            let Some(sig) = v["signature"].as_str().and_then(|s| hex::decode(s).ok()) else { continue };
            if let Ok(b) = Beacon::verify(round, &sig) {
                return Some(b);
            }
        }
        None
    })
    .await
    .ok()
    .flatten()
}

#[derive(Serialize)]
pub struct Proof {
    pub auctions: usize,
    pub sealed_bids: usize,
    pub settled_honestly: usize,
    pub mismatches: usize,
    /// Zatoshi deposited as bonds across every auction.
    pub bonded: u64,
    pub rows: Vec<Row>,
}

#[derive(Serialize)]
pub struct Row {
    pub id: String,
    pub item: String,
    pub verdict: String,
    pub deposits: usize,
    pub price: Option<u64>,
    pub settlement: Option<String>,
    pub audited: i64,
}

pub async fn proof(State(s): State<AppState>) -> Result<Json<Proof>, ApiError> {
    let db = s.db.lock().unwrap();
    let mut stmt = db.prepare(
        "SELECT a.id, a.terms, r.verdict, r.report, r.updated FROM auctions a
         LEFT JOIN audits r ON r.id = a.id ORDER BY a.created DESC LIMIT 200",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<i64>>(4)?,
        ))
    })?;
    let mut out = Proof { auctions: 0, sealed_bids: 0, settled_honestly: 0, mismatches: 0, bonded: 0, rows: vec![] };
    for row in rows {
        let (id, terms, verdict, report, updated) = row?;
        let item = serde_json::from_str::<Value>(&terms).ok().and_then(|t| t["item"].as_str().map(String::from)).unwrap_or_default();
        let report: Option<Value> = report.and_then(|r| serde_json::from_str(&r).ok());
        let deposits = report.as_ref().and_then(|r| r["deposits"].as_array().map(|d| d.len())).unwrap_or(0);
        let bonded: u64 = report
            .as_ref()
            .and_then(|r| r["deposits"].as_array().map(|d| d.iter().filter_map(|x| x["value"].as_u64()).sum()))
            .unwrap_or(0);
        let verdict = verdict.unwrap_or_else(|| "pending".into());
        out.auctions += 1;
        out.sealed_bids += deposits;
        out.bonded += bonded;
        out.settled_honestly += (verdict == "honest") as usize;
        out.mismatches += (verdict == "mismatch") as usize;
        out.rows.push(Row {
            id,
            item,
            price: report.as_ref().and_then(|r| r["outcome"]["price"].as_u64()),
            settlement: report.as_ref().and_then(|r| r["settlement"]["txid"].as_str().map(String::from)),
            verdict,
            deposits,
            audited: updated.unwrap_or(0),
        });
    }
    Ok(Json(out))
}
