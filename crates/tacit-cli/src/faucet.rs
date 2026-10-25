//! `tacit faucet`: a testnet faucet built into a Tacit node, so anyone who
//! opens the web app can get test coins in one click.
//!
//! It holds an ordinary shielded wallet (this home's personal one) and sends
//! a fixed drip to any testnet unified address with a shielded receiver, at
//! most once a day per address and per IP. Drips are fully shielded, like
//! every other Tacit transaction. It refuses to run on mainnet.
//!
//! The faucet listens on a local port; the coordinator relays
//! `/v1/faucet/status` and `/v1/faucet/drip` to it.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Result};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tacit_wallet::{ironwood_proving_key, prove, raw_receiver, sign_with_usk};
use tokio::sync::{mpsc, oneshot};

use crate::home::Home;
use crate::net::NetName;
use crate::{parse_zec, text_memo, zec};

const DAY: u64 = 24 * 60 * 60;

#[derive(Default, Serialize, Deserialize)]
struct Ledger {
    /// Last drip per address and per IP, unix seconds.
    addresses: HashMap<String, u64>,
    ips: HashMap<String, u64>,
    drips: u64,
}

#[derive(Clone, Serialize)]
struct Status {
    network: String,
    address: String,
    /// Transparent address public faucets can pay; coins there are shielded
    /// into Ironwood automatically.
    deposit_address: String,
    balance: u64,
    drip: u64,
    drips: u64,
    synced_height: u32,
}

struct Shared {
    status: Mutex<Status>,
    ledger: Mutex<Ledger>,
    ledger_path: std::path::PathBuf,
    net: NetName,
    jobs: mpsc::Sender<(String, oneshot::Sender<Result<String, String>>)>,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

pub async fn run(home: &Home, listen: String, amount: String) -> Result<()> {
    if home.net == NetName::Main {
        bail!("the faucet only runs on testnet or regtest");
    }
    let drip = parse_zec(&amount)?;
    let ledger_path = home.dir.join("faucet.json");
    let ledger: Ledger = std::fs::read_to_string(&ledger_path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let (mut wallet, account) = home.personal()?;
    let address = wallet.next_address(account)?;
    let deposit_address = wallet.transparent_address(&home.usk()?)?;
    let (jobs, rx) = mpsc::channel(64);
    let shared = Arc::new(Shared {
        status: Mutex::new(Status {
            network: home.net.to_string(),
            address: address.clone(),
            deposit_address: deposit_address.clone(),
            balance: 0,
            drip,
            drips: ledger.drips,
            synced_height: 0,
        }),
        ledger: Mutex::new(ledger),
        ledger_path,
        net: home.net,
        jobs,
    });
    drop(wallet);

    // The wallet lives on its own thread: it syncs every minute and sends
    // drips one at a time.
    let (dir, net, worker_shared) = (home.dir.clone(), home.net, shared.clone());
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
        rt.block_on(worker(dir, net, drip, worker_shared, rx));
    });

    let app = Router::new()
        .route("/status", get(status))
        .route("/drip", post(drip_handler))
        .with_state(shared);
    let listener = tokio::net::TcpListener::bind(&listen).await?;
    println!("testnet faucet on http://{listen}, {} per drip", zec(drip));
    println!("  shielded address  {address}");
    println!("  deposit address   {deposit_address} (shielded automatically)");
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await?;
    Ok(())
}

async fn worker(
    dir: std::path::PathBuf,
    net: NetName,
    drip: u64,
    shared: Arc<Shared>,
    mut rx: mpsc::Receiver<(String, oneshot::Sender<Result<String, String>>)>,
) {
    let home = Home::open(dir, net).expect("faucet home");
    let pk = ironwood_proving_key();
    let usk = home.usk().expect("faucet key");
    let mut last_sync = std::time::Instant::now() - Duration::from_secs(3600);
    loop {
        // Sync at least once a minute, and before every drip.
        let job = tokio::time::timeout(Duration::from_secs(60), rx.recv()).await;
        let job = match job {
            Ok(None) => return,
            Ok(Some(j)) => Some(j),
            Err(_) => None,
        };
        let res: Result<(String, u64, u32)> = async {
            let mut client = home.client().await?;
            let (mut wallet, account) = home.personal()?;
            if job.is_some() || last_sync.elapsed() > Duration::from_secs(50) {
                wallet.sync(&mut client).await?;
                last_sync = std::time::Instant::now();
                // Move anything public faucets paid in into Ironwood.
                if let Some(txid) = wallet.shield(&mut client, account, &usk, &pk).await? {
                    println!("faucet: shielded deposits in {txid}");
                }
            }
            let mut txid = String::new();
            if let Some((to, _)) = &job {
                let memo = text_memo("Tacit testnet faucet. Go seal a bid.")?;
                let pczt = wallet.propose(account, to, drip, Some(memo))?;
                let pczt = sign_with_usk(prove(pczt, &pk)?, &usk)?;
                txid = wallet.finish_and_send(&mut client, pczt).await?.to_string();
            }
            Ok((txid, wallet.spendable(account)?, wallet.synced_height()?.unwrap_or(0)))
        }
        .await;
        match res {
            Ok((txid, balance, height)) => {
                {
                    let mut s = shared.status.lock().unwrap();
                    s.balance = balance;
                    s.synced_height = height;
                }
                if let Some((_, reply)) = job {
                    let _ = reply.send(Ok(txid));
                }
            }
            Err(e) => {
                eprintln!("faucet: {e:#}");
                if let Some((_, reply)) = job {
                    let _ = reply.send(Err(format!("{e:#}")));
                }
            }
        }
    }
}

async fn status(State(s): State<Arc<Shared>>) -> Json<Status> {
    Json(s.status.lock().unwrap().clone())
}

#[derive(Deserialize)]
struct DripRequest {
    address: String,
}

#[derive(Serialize)]
struct DripReply {
    txid: String,
    amount: u64,
}

type ApiResult<T> = Result<Json<T>, (StatusCode, Json<serde_json::Value>)>;

fn refuse(code: StatusCode, msg: impl Into<String>) -> (StatusCode, Json<serde_json::Value>) {
    (code, Json(serde_json::json!({ "error": msg.into() })))
}

async fn drip_handler(
    State(s): State<Arc<Shared>>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<DripRequest>,
) -> ApiResult<DripReply> {
    let address = req.address.trim().to_string();
    let net = s.net.to_string();
    let params = s.net.params();
    if raw_receiver(&params, &address).is_none() {
        return Err(refuse(StatusCode::BAD_REQUEST, format!("that isn't a shielded {net} unified address")));
    }
    // Behind the coordinator (and Railway's proxy), the client's IP is the
    // first X-Forwarded-For entry.
    let ip = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|v| v.trim().to_string())
        .unwrap_or_else(|| peer.ip().to_string());
    let t = now();
    {
        let l = s.ledger.lock().unwrap();
        let wait = |last: Option<&u64>| last.map(|l| (l + DAY).saturating_sub(t)).filter(|w| *w > 0);
        if let Some(w) = wait(l.addresses.get(&address)).or(wait(l.ips.get(&ip))) {
            return Err(refuse(
                StatusCode::TOO_MANY_REQUESTS,
                format!("one drip a day. Try again in {} h {} min", w / 3600, (w % 3600) / 60),
            ));
        }
    }
    let (balance, drip) = {
        let st = s.status.lock().unwrap();
        (st.balance, st.drip)
    };
    if balance < drip + 20_000 {
        return Err(refuse(StatusCode::SERVICE_UNAVAILABLE, "the faucet is empty right now. Donations welcome at its address"));
    }
    let (tx, rx) = oneshot::channel();
    s.jobs
        .send((address.clone(), tx))
        .await
        .map_err(|_| refuse(StatusCode::SERVICE_UNAVAILABLE, "faucet is restarting"))?;
    match rx.await {
        Ok(Ok(txid)) => {
            let mut l = s.ledger.lock().unwrap();
            l.addresses.insert(address, t);
            l.ips.insert(ip, t);
            l.drips += 1;
            s.status.lock().unwrap().drips = l.drips;
            let _ = std::fs::write(&s.ledger_path, serde_json::to_string(&*l).unwrap_or_default());
            Ok(Json(DripReply { txid, amount: drip }))
        }
        Ok(Err(e)) => Err(refuse(StatusCode::BAD_GATEWAY, format!("couldn't send: {e}"))),
        Err(_) => Err(refuse(StatusCode::SERVICE_UNAVAILABLE, "faucet is restarting")),
    }
}
