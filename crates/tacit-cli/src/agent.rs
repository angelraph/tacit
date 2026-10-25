//! `tacit agent`: an always-online operator or arbiter seat.
//!
//! Sellers and escrow parties use the web app, which can't sit online for
//! hours waiting for a drand round. An agent can. It announces itself on the
//! coordinator so the app can offer it as a seat, then loops:
//!
//! - **key generation**: it joins every `dkg` room it is seated in;
//! - **settling** (operator): once an auction on one of its vaults has
//!   unsealed, it computes the settlement, proves it and asks the arbiter
//!   seat to cosign;
//! - **cosigning**: in a `settle:` room it signs only if the transaction pays
//!   exactly what the auction rules require; in an `escrow:` room it signs
//!   only the releases the escrow rules let an unattended arbiter approve
//!   (buyer pays seller, seller refunds buyer). Anything else is a dispute and
//!   is left for a human.
//!
//! An agent never signs anything it hasn't recomputed itself, so the worst a
//! dishonest co-seat can get from it is a refusal.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use tacit_core::envelope::SeatKey;
use tacit_core::escrow::{self, EscrowTerms, Release};
use tacit_wallet::auction::check_settlement;
use tacit_wallet::relay;

use crate::home::Home;
use crate::market::{auctions_on_our_vaults, cosign_auction, ready_to_settle, settle_auction};
use crate::vault::{blocking, keygen, SEAT_TIMEOUT};

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Role {
    Operator,
    Arbiter,
}

impl Role {
    fn name(self) -> &'static str {
        match self {
            Role::Operator => "operator",
            Role::Arbiter => "arbiter",
        }
    }
}

/// What the agent has already dealt with, so a restart doesn't redo it.
#[derive(Default, Serialize, Deserialize)]
struct State {
    rooms: BTreeSet<String>,
    settled: BTreeSet<String>,
}

const POLL: Duration = Duration::from_secs(3);
const ANNOUNCE: Duration = Duration::from_secs(60);
const RETRY: Duration = Duration::from_secs(60);

pub async fn run(home: &Home, role: Role, name: String, run_by: String) -> Result<()> {
    let me = home.seat_key()?.public();
    let path = home.dir.join(format!("agent-{}.json", role.name()));
    let mut state: State = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let save = |s: &State| std::fs::write(&path, serde_json::to_string_pretty(s).unwrap());
    println!("{} agent {name:?} on {}", role.name(), home.net);
    println!("seat key {}", hex::encode(me));
    println!("coordinator {}", home.cfg.coordinator);

    let mut announced: Option<Instant> = None;
    // Auctions with nothing to settle yet, and when to look again.
    let mut retry: std::collections::HashMap<String, Instant> = Default::default();
    loop {
        if announced.is_none_or(|t| t.elapsed() > ANNOUNCE) {
            let (role_s, name_c, by_c) = (role.name().to_string(), name.clone(), run_by.clone());
            match blocking(home.relay(), move |r| Ok(r.announce(&me, &role_s, &name_c, &by_c)?)).await {
                Ok(()) => announced = Some(Instant::now()),
                Err(e) => eprintln!("can't reach the coordinator: {e:#}"),
            }
        }

        // New rooms we sit in.
        let rooms = match blocking(home.relay(), move |r| Ok(r.rooms_for(&me, 0)?)).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("listing rooms: {e:#}");
                vec![]
            }
        };
        for (room, seats, purpose, _) in rooms {
            if !state.rooms.insert(room.clone()) {
                continue;
            }
            save(&state)?;
            // Signing rooms name the seat that coordinates them; that's us
            // when we opened it, and there's nothing to cosign.
            let from = purpose.rsplit(':').next().and_then(|n| n.parse::<usize>().ok());
            if purpose != "dkg" && from.and_then(|n| seats.get(n.wrapping_sub(1))) == Some(&me) {
                continue;
            }
            println!("room {room}: {}", if purpose.is_empty() { "(no purpose)" } else { &purpose });
            spawn_job(home, role, room, purpose);
        }

        // The operator settles auctions on its vaults once they unseal.
        if role == Role::Operator {
            match auctions_on_our_vaults(home).await {
                Ok(list) => {
                    for (id, vault, terms) in list {
                        if state.settled.contains(&id) || retry.get(&id).is_some_and(|t| Instant::now() < *t) {
                            continue;
                        }
                        if !ready_to_settle(home, &terms).await.unwrap_or(false) {
                            continue;
                        }
                        let v = home.load_vault(&vault)?;
                        // Seats are seller, operator, arbiter: ask the arbiter.
                        let with = if v.seat == 3 { 2 } else { 3 };
                        println!("auction {id}: bids are open, settling with seat {with}");
                        state.settled.insert(id.clone());
                        save(&state)?;
                        match settle_auction(home, &id, &vault, with).await {
                            Ok(txid) => println!("auction {id}: settled in {txid}"),
                            Err(e) => {
                                // Nothing deposited yet, a cosigner that is
                                // slow to answer, or a node hiccup: try again
                                // in a minute.
                                state.settled.remove(&id);
                                save(&state)?;
                                // An auction that closed with no deposits has
                                // nothing to do unless someone pays in late (and
                                // is owed a refund), so look rarely.
                                let idle = format!("{e:#}").contains("received no deposits");
                                if idle {
                                    println!("auction {id}: closed with no deposits");
                                } else {
                                    eprintln!("auction {id}: not settled yet: {e:#}");
                                }
                                retry.insert(id.clone(), Instant::now() + if idle { RETRY * 30 } else { RETRY });
                            }
                        }
                    }
                }
                Err(e) => eprintln!("listing auctions: {e:#}"),
            }
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Runs one room's job on its own thread (key generation and signing block
/// for as long as the other seats take).
fn spawn_job(home: &Home, role: Role, room: String, purpose: String) {
    let (dir, net) = (home.dir.clone(), home.net);
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("runtime");
        let res = rt.block_on(async move {
            let home = Home::open(dir, net)?;
            job(&home, role, &room, &purpose).await
        });
        if let Err(e) = res {
            eprintln!("job failed: {e:#}");
        }
    });
}

async fn job(home: &Home, role: Role, room: &str, purpose: &str) -> Result<()> {
    let parts: Vec<&str> = purpose.split(':').collect();
    match parts.as_slice() {
        ["dkg"] => {
            let name = format!("v{}", &room[..12.min(room.len())]);
            keygen(home, &name, room).await?;
            println!("room {room}: vault {name} ready");
        }
        ["settle", id, from] => {
            let from: u16 = from.parse().context("bad seat in room purpose")?;
            let ours = auctions_on_our_vaults(home).await?;
            let (_, vault, _) = ours
                .iter()
                .find(|(a, _, _)| a == id)
                .ok_or_else(|| anyhow!("auction {id} isn't on one of our vaults"))?;
            cosign_auction(home, id, vault, room, from, true).await?;
            println!("room {room}: cosigned the settlement of {id}");
        }
        ["escrow", id, payee, from] => {
            let to = Release::parse(payee).ok_or_else(|| anyhow!("unknown payee {payee}"))?;
            let from: u16 = from.parse().context("bad seat in room purpose")?;
            cosign_escrow(home, role, id, room, to, from).await?;
            println!("room {room}: cosigned escrow {id} paying the {payee}");
        }
        _ => println!("room {room}: nothing to do for {purpose:?}"),
    }
    Ok(())
}

/// Cosigns an escrow release if the escrow rules allow an unattended
/// arbiter to, and the transaction pays exactly the release amount.
/// An arbiter's decision on a dispute: cosigns the payout requested in
/// `room` after checking it pays exactly what the escrow holds.
pub async fn decide(home: &Home, room: &str) -> Result<()> {
    let me = home.seat_key()?.public();
    let rooms = blocking(home.relay(), move |r| Ok(r.rooms_for(&me, 0)?)).await?;
    let (_, _, purpose, _) =
        rooms.into_iter().find(|(id, ..)| id == room).ok_or_else(|| anyhow!("we have no seat in room {room}"))?;
    let parts: Vec<&str> = purpose.split(':').collect();
    let ["escrow", id, payee, from] = parts.as_slice() else {
        bail!("room {room} isn't an escrow payout request ({purpose:?})");
    };
    let to = Release::parse(payee).ok_or_else(|| anyhow!("unknown payee {payee}"))?;
    let from: u16 = from.parse().context("bad seat in room purpose")?;
    println!("deciding escrow {id}: pay the {payee} (requested by seat {from})");
    cosign_escrow(home, Role::Operator, id, room, to, from).await?;
    println!("signed; the requester broadcasts the payout");
    Ok(())
}

async fn cosign_escrow(home: &Home, role: Role, id: &str, room: &str, to: Release, from: u16) -> Result<()> {
    let id_c = id.to_string();
    let json = blocking(home.relay(), move |r| Ok(r.escrow(&id_c)?)).await?;
    let terms: EscrowTerms = serde_json::from_value(json).context("coordinator sent malformed escrow terms")?;
    if hex::encode(terms.id()) != id {
        bail!("escrow terms don't hash to {id}");
    }
    terms.validate()?;
    let me = hex::encode(home.seat_key()?.public());
    let seat = terms.seat_of(&me).ok_or_else(|| anyhow!("we have no seat in escrow {id}"))?;
    if role == Role::Arbiter && seat == escrow::ARBITER && !escrow::arbiter_auto_approves(from, to) {
        bail!(
            "dispute in escrow {id}: seat {from} asks to pay the {}. Review it, then run `tacit decide {room}` to rule for them",
            to.name()
        );
    }
    let name = home
        .vault_names()?
        .into_iter()
        .find(|n| home.load_vault(n).map(|v| v.vault.ufvk().encode(&home.params) == terms.vault_ufvk).unwrap_or(false))
        .ok_or_else(|| anyhow!("escrow {id} isn't on one of our vaults"))?;
    let v = home.load_vault(&name)?;
    let mut client = home.client().await?;
    let (mut wallet, account) = v.wallet(home, &mut client).await?;
    wallet.sync(&mut client).await?;
    let held: Vec<u64> = wallet.received(account)?.iter().map(|r| r.value).collect();
    let (addr, amount) = escrow::payout(&terms, &held, to).ok_or_else(|| anyhow!("escrow {id} holds too little to pay out"))?;
    let expected = vec![(addr, amount)];
    let n = held.len();
    println!("escrow {id}: paying {amount} zat to the {}", to.name());

    let (key, seats, kp, params) = (home.seat_key()?.to_bytes(), v.seats.clone(), v.key_package.clone(), home.params.clone());
    let coordinator = *v.seats.get(from as usize - 1).ok_or_else(|| anyhow!("no seat {from}"))?;
    let room = room.to_string();
    blocking(home.relay(), move |r| {
        let approve = |p: &pczt::Pczt| match check_settlement(&params, p, &expected, n) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("refusing to sign escrow payout: {e}");
                false
            }
        };
        Ok(relay::run_signer(&r, SeatKey::from_bytes(key), &room, seats, coordinator, kp, approve, SEAT_TIMEOUT)?)
    })
    .await
}
