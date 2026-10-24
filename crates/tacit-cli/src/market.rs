//! `tacit auction ...` and `tacit bid`.
//!
//! Auction terms live on the coordinator, but nothing here trusts it: every
//! command recomputes the auction id from the terms it was handed, and the
//! seats recompute the settlement themselves before signing it.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use clap::Subcommand;
use tacit_core::auction::{self, PriceRule, Rejection, Terms};
use tacit_core::beacon::Beacon;
use tacit_core::seal;
use tacit_core::envelope::SeatKey;
use tacit_wallet::auction::{check_settlement, deposits, place_bid, settlement_payments, verify};
use tacit_wallet::relay;
use tacit_wallet::{drand, ironwood_proving_key, prove, GrpcClient, Wallet};
use zcash_keys::keys::{UnifiedAddressRequest, UnifiedFullViewingKey};

use crate::home::{Home, VaultSeat};
use crate::net::Net;
use crate::vault::{blocking, SEAT_TIMEOUT};
use crate::{confirm, parse_zec, zec};

#[derive(Subcommand)]
pub enum AuctionCmd {
    /// Publish a new auction backed by one of your vaults. You are the seller.
    Create {
        /// Vault the bonds are paid into.
        #[arg(long)]
        vault: String,
        /// What is being sold (at most 256 bytes).
        #[arg(long)]
        item: String,
        /// Lowest acceptable price in ZEC.
        #[arg(long)]
        reserve: String,
        /// Bond every bidder deposits, in ZEC; also the highest possible bid.
        /// Must be 1, 2 or 5 times a power of ten.
        #[arg(long)]
        bond: String,
        /// Bids are accepted for this many blocks from now.
        #[arg(long, default_value_t = 10)]
        bid_blocks: u32,
        /// Bids unseal this many seconds from now (the drand round they're sealed to).
        #[arg(long, default_value_t = 1800)]
        close_in: u64,
        /// Winner pays their own bid instead of the second highest.
        #[arg(long)]
        first_price: bool,
    },
    /// Auctions published on the coordinator.
    List,
    /// An auction's terms and where it stands.
    Show { id: String },
    /// Once bids unseal: compute the settlement, prove it, and sign it with a
    /// second seat. Run by a seat of the auction's vault.
    Settle {
        id: String,
        #[arg(long)]
        vault: String,
        /// Seat number (1 to 3) of the seat that cosigns.
        #[arg(long)]
        with: u16,
    },
    /// Cosign a settlement after checking it pays exactly what the rules say.
    Cosign {
        id: String,
        #[arg(long)]
        vault: String,
        /// Signing room printed by `tacit auction settle`.
        #[arg(long)]
        room: String,
        /// Seat number of the seat running the settlement.
        #[arg(long)]
        from: u16,
        /// Sign without asking if the settlement checks out.
        #[arg(long)]
        yes: bool,
    },
    /// Audit an auction from its public terms alone: rebuild the settlement
    /// the rules demand and compare it with what the vault paid.
    Verify { id: String },
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

/// Fetches terms and checks they hash to the id we asked for.
async fn fetch_terms(home: &Home, id: &str) -> Result<Terms> {
    let id_owned = id.to_string();
    let json = blocking(home.relay(), move |r| Ok(r.auction(&id_owned)?)).await?;
    let terms: Terms = serde_json::from_value(json).context("coordinator sent malformed terms")?;
    if hex::encode(terms.id()) != id.to_lowercase() {
        bail!("the coordinator's terms for {id} hash to a different id; refusing to use them");
    }
    if terms.network != home.net.to_string() {
        bail!("auction {id} is on {}, this home is {}", terms.network, home.net);
    }
    terms.validate()?;
    Ok(terms)
}

fn vault_address(home: &Home, terms: &Terms) -> Result<String> {
    let ufvk = UnifiedFullViewingKey::decode(&home.params, &terms.vault_ufvk)
        .map_err(|e| anyhow!("terms carry a bad viewing key: {e}"))?;
    let (ua, _) = ufvk
        .default_address(UnifiedAddressRequest::ORCHARD)
        .map_err(|e| anyhow!("vault viewing key gives no address: {e:?}"))?;
    Ok(ua.encode(&home.params))
}

fn our_vault(home: &Home, name: &str, terms: &Terms) -> Result<VaultSeat> {
    let v = home.load_vault(name)?;
    if v.vault.ufvk().encode(&home.params) != terms.vault_ufvk {
        bail!("this auction isn't backed by vault {name}");
    }
    Ok(v)
}

async fn tip(client: &mut GrpcClient) -> Result<u32> {
    Ok(Wallet::<Net>::tip(client).await?)
}

async fn wait_for_beacon(round: u64) -> Result<Beacon> {
    let secs = seal::round_time(round).saturating_sub(now());
    if secs > 0 {
        println!("waiting {secs} s for drand round {round}, when the bids unseal...");
    }
    Ok(tokio::task::spawn_blocking(move || drand::wait_for(round)).await?)
}

/// What the rules say the vault must pay, computed from our own view of the
/// chain. Returns the payments and how many deposits they settle.
async fn expected_settlement(
    home: &Home,
    v: &VaultSeat,
    terms: &Terms,
    client: &mut GrpcClient,
) -> Result<(Wallet<Net>, zcash_client_sqlite::AccountUuid, auction::Outcome, Vec<(String, u64)>, usize)> {
    let beacon = wait_for_beacon(terms.close_round).await?;
    let (mut wallet, account) = v.wallet(home, client).await?;
    wallet.sync(client).await?;
    let deps = deposits(&wallet, account)?;
    if deps.is_empty() {
        bail!("the vault has received no deposits, so there is nothing to settle");
    }
    let outcome = auction::settle(terms, &deps, &beacon)?;
    let payments = settlement_payments(&home.params, terms, &outcome);
    Ok((wallet, account, outcome, payments, deps.len()))
}

fn print_outcome(outcome: &auction::Outcome, payments: &[(String, u64)]) {
    for d in &outcome.opened {
        let what = match (&d.bid, &d.rejected) {
            (Some(b), None) => format!("bid {}", zec(*b)),
            (b, Some(r)) => format!(
                "{} ({})",
                b.map(zec).unwrap_or("no readable bid".into()),
                match r {
                    Rejection::Unreadable => "unreadable, goes to the seller",
                    Rejection::Late => "late, refunded",
                    Rejection::WrongBond => "wrong bond, refunded",
                    Rejection::BidAboveBond => "above the bond, refunded",
                    Rejection::BelowReserve => "below the reserve, refunded",
                }
            ),
            (None, None) => "no bid".into(),
        };
        println!("  deposit {}  {what}", hex::encode(&d.deposit_id[..6]));
    }
    match outcome.price {
        Some(p) => println!("  clearing price {}, network fee {}", zec(p), zec(outcome.fee)),
        None => println!("  no valid bid; everyone is refunded"),
    }
    for (to, amount) in payments {
        println!("  pays {:>16}  to {}...", zec(*amount), &to[..24.min(to.len())]);
    }
}

/// Computes, proves and coordinates the settlement of auction `id` from our
/// seat in `vault`, cosigned by seat `with`. Returns the broadcast txid.
pub async fn settle_auction(home: &Home, id: &str, vault: &str, with: u16) -> Result<String> {
    let terms = fetch_terms(home, id).await?;
    let v = our_vault(home, vault, &terms)?;
    if with == v.seat || !(1..=3).contains(&with) {
        bail!("--with must be one of the other two seats");
    }
    let mut client = home.client().await?;
    let (mut wallet, account, outcome, payments, n) = expected_settlement(home, &v, &terms, &mut client).await?;
    print_outcome(&outcome, &payments);

    let to_pay: Vec<_> = payments.iter().map(|(to, a)| (to.clone(), *a, None)).collect();
    let pczt = wallet.propose_payments(account, &to_pay)?;
    println!("proving the settlement...");
    let pczt = prove(pczt, &ironwood_proving_key())?;
    check_settlement(&home.params, &pczt, &payments, n).map_err(|e| anyhow!("our own settlement fails the check: {e}"))?;

    let seats = v.seats.clone();
    let purpose = format!("settle:{id}:{}", v.seat);
    let room = blocking(home.relay(), move |r| Ok(r.open_room_for(&seats, &purpose)?)).await?;
    println!("\nsigning room {room}");
    println!("seat {with} runs:  tacit auction cosign {id} --vault <name> --room {room} --from {}", v.seat);
    let (key, seats, vault_k, kp, me) =
        (home.seat_key()?.to_bytes(), v.seats.clone(), v.vault.clone(), v.key_package.clone(), v.seat);
    let signed = blocking(home.relay(), move |r| {
        Ok(relay::run_coordinator(&r, SeatKey::from_bytes(key), &room, seats, vault_k, pczt, &[me, with], Some(kp), SEAT_TIMEOUT)?)
    })
    .await?;
    let txid = wallet.finish_and_send(&mut client, signed).await?;
    Ok(txid.to_string())
}

/// Cosigns a settlement another seat coordinates in `room`, after checking
/// it pays exactly what the rules require. `yes` skips the prompt.
pub async fn cosign_auction(home: &Home, id: &str, vault: &str, room: &str, from: u16, yes: bool) -> Result<()> {
    let terms = fetch_terms(home, id).await?;
    let v = our_vault(home, vault, &terms)?;
    if !(1..=3).contains(&from) || from == v.seat {
        bail!("--from must be one of the other two seats");
    }
    let mut client = home.client().await?;
    let (_, _, outcome, payments, n) = expected_settlement(home, &v, &terms, &mut client).await?;
    println!("the rules say this auction settles as:");
    print_outcome(&outcome, &payments);
    println!("waiting for seat {from} to send the settlement...");

    let (key, seats, kp, params) = (home.seat_key()?.to_bytes(), v.seats.clone(), v.key_package.clone(), home.params.clone());
    let coordinator = v.seats[from as usize - 1];
    let room = room.to_string();
    blocking(home.relay(), move |r| {
        let approve = |p: &pczt::Pczt| match check_settlement(&params, p, &payments, n) {
            Ok(()) => {
                println!("the settlement pays exactly that.");
                yes || confirm("sign it?")
            }
            Err(e) => {
                println!("refusing to sign: {e}");
                false
            }
        };
        Ok(relay::run_signer(&r, SeatKey::from_bytes(key), &room, seats, coordinator, kp, approve, SEAT_TIMEOUT)?)
    })
    .await
}

/// The auction's terms if it is backed by one of our vaults: (vault name, terms).
pub async fn auctions_on_our_vaults(home: &Home) -> Result<Vec<(String, String, Terms)>> {
    let list = blocking(home.relay(), |r| Ok(r.auctions()?)).await?;
    let mut ours = Vec::new();
    let vaults: Vec<(String, String)> = home
        .vault_names()?
        .into_iter()
        .filter_map(|n| home.load_vault(&n).ok().map(|v| (n, v.vault.ufvk().encode(&home.params))))
        .collect();
    for (id, json) in list {
        let Ok(t) = serde_json::from_value::<Terms>(json) else { continue };
        if t.network != home.net.to_string() || hex::encode(t.id()) != id {
            continue;
        }
        if let Some((name, _)) = vaults.iter().find(|(_, ufvk)| *ufvk == t.vault_ufvk) {
            ours.push((id, name.clone(), t));
        }
    }
    Ok(ours)
}

/// Whether an auction's bids have unsealed and bidding is over, so it can
/// be settled now.
pub async fn ready_to_settle(home: &Home, terms: &Terms) -> Result<bool> {
    if now() < seal::round_time(terms.close_round) {
        return Ok(false);
    }
    let mut client = home.client().await?;
    Ok(tip(&mut client).await? > terms.bids_until_height)
}

pub async fn run(home: &Home, cmd: AuctionCmd) -> Result<()> {
    match cmd {
        AuctionCmd::Create { vault, item, reserve, bond, bid_blocks, close_in, first_price } => {
            let v = home.load_vault(&vault)?;
            let mut client = home.client().await?;
            let (mut wallet, account) = home.personal()?;
            let seller = wallet.next_address(account)?;
            let height = tip(&mut client).await?;
            if home.net != crate::net::NetName::Regtest && close_in < bid_blocks as u64 * 75 {
                println!("note: bids unseal before the bidding window can end ({bid_blocks} blocks is about {} s)", bid_blocks as u64 * 75);
            }
            let terms = Terms {
                network: home.net.to_string(),
                vault_ufvk: v.vault.ufvk().encode(&home.params),
                seller,
                item,
                created_height: v.birthday,
                reserve: parse_zec(&reserve)?,
                bond: parse_zec(&bond)?,
                bids_until_height: height + bid_blocks,
                close_round: seal::round_at(now() + close_in),
                rule: if first_price { PriceRule::FirstPrice } else { PriceRule::SecondPrice },
            };
            terms.validate()?;
            let json = serde_json::to_value(&terms)?;
            let id = blocking(home.relay(), move |r| Ok(r.publish_auction(&json)?)).await?;
            if id != hex::encode(terms.id()) {
                bail!("coordinator returned id {id}, but the terms hash to {}", hex::encode(terms.id()));
            }
            println!("auction {id}");
            println!("  bids until height {}, unsealing at drand round {}", terms.bids_until_height, terms.close_round);
            println!("bidders run:  tacit bid {id} <amount>");
            Ok(())
        }
        AuctionCmd::List => {
            let list = blocking(home.relay(), |r| Ok(r.auctions()?)).await?;
            for (id, json) in list {
                let Ok(t) = serde_json::from_value::<Terms>(json) else { continue };
                if t.network != home.net.to_string() || hex::encode(t.id()) != id {
                    continue;
                }
                println!("{id}  bond {:<12}  reserve {:<12}  {}", zec(t.bond), zec(t.reserve), t.item);
            }
            Ok(())
        }
        AuctionCmd::Show { id } => {
            let terms = fetch_terms(home, &id).await?;
            let mut client = home.client().await?;
            let height = tip(&mut client).await?;
            println!("auction      {id}");
            println!("item         {}", terms.item);
            println!("rule         {}", match terms.rule {
                PriceRule::FirstPrice => "first price",
                PriceRule::SecondPrice => "second price",
            });
            println!("reserve      {}", zec(terms.reserve));
            println!("bond         {}", zec(terms.bond));
            println!("vault        {}", vault_address(home, &terms)?);
            println!(
                "bidding      until height {} (now {height}{})",
                terms.bids_until_height,
                if height > terms.bids_until_height { ", closed" } else { ", open" }
            );
            let at = seal::round_time(terms.close_round);
            println!(
                "unseals      drand round {} ({})",
                terms.close_round,
                if now() >= at { "now readable".to_string() } else { format!("in {} s", at - now()) }
            );
            Ok(())
        }
        AuctionCmd::Settle { id, vault, with } => {
            let txid = settle_auction(home, &id, &vault, with).await?;
            println!("settled: {txid}");
            Ok(())
        }
        AuctionCmd::Cosign { id, vault, room, from, yes } => {
            cosign_auction(home, &id, &vault, &room, from, yes).await?;
            println!("signed; seat {from} broadcasts the settlement");
            Ok(())
        }
        AuctionCmd::Verify { id } => {
            let terms = fetch_terms(home, &id).await?;
            let beacon = wait_for_beacon(terms.close_round).await?;
            let mut client = home.client().await?;
            let dir = home.dir.join("audits");
            std::fs::create_dir_all(&dir)?;
            let db = dir.join(format!("{}.sqlite", &id[..16.min(id.len())]));
            // Start from nothing every time: an audit must not lean on old state.
            let _ = std::fs::remove_file(&db);
            println!("scanning the vault from height {} with its published viewing key...", terms.created_height);
            let verdict = verify(home.params.clone(), &mut client, &terms, &beacon, &db).await?;
            print_outcome(&verdict.outcome, &settlement_payments(&home.params, &terms, &verdict.outcome));
            match verdict.settlement {
                None => println!("the vault hasn't settled yet"),
                Some(tx) => println!("settlement {tx}"),
            }
            for (to, a) in &verdict.missing {
                println!("  MISSING     {} to {to}", zec(*a));
            }
            for (to, a) in &verdict.unexpected {
                println!("  UNEXPECTED  {} to {to}", zec(*a));
            }
            if verdict.is_honest() {
                println!("verified: the vault paid exactly what the rules require");
                Ok(())
            } else if verdict.settlement.is_none() {
                Ok(())
            } else {
                bail!("the settlement does not match the rules")
            }
        }
    }
}

pub async fn bid(home: &Home, id: &str, amount: &str) -> Result<()> {
    let terms = fetch_terms(home, id).await?;
    let amount = parse_zec(amount)?;
    if amount < terms.reserve || amount > terms.bond {
        bail!("bid must be between the reserve {} and the bond {}", zec(terms.reserve), zec(terms.bond));
    }
    if now() >= seal::round_time(terms.close_round) {
        bail!("this auction's bids have already unsealed");
    }
    let mut client = home.client().await?;
    let height = tip(&mut client).await?;
    if height >= terms.bids_until_height {
        bail!("bidding closed at height {}; a deposit now would only be refunded", terms.bids_until_height);
    }
    let to = vault_address(home, &terms)?;
    let (mut wallet, account) = home.personal()?;
    wallet.sync(&mut client).await?;
    println!("sealing a {} bid to drand round {} and proving the {} deposit...", zec(amount), terms.close_round, zec(terms.bond));
    let pk = ironwood_proving_key();
    let txid = place_bid(&mut wallet, &mut client, &pk, account, &home.usk()?, &terms, &to, amount).await?;
    println!("bid placed: {txid}");
    println!("nobody, you included, can read it before round {}", terms.close_round);
    Ok(())
}
