//! `tacit vault ...`: opening a 2 of 3 vault with two other people.

use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use clap::Subcommand;
use tacit_core::envelope::SeatKey;
use tacit_vault::session::SeatPub;
use tacit_wallet::relay::{self, Relay};
use tacit_wallet::Wallet;

use crate::home::Home;
use crate::net::Net;
use crate::zec;

/// How long a seat waits for the others during key generation or signing.
pub const SEAT_TIMEOUT: Duration = Duration::from_secs(15 * 60);

#[derive(Subcommand)]
pub enum VaultCmd {
    /// Open a vault room with three seats and run key generation. The other
    /// two seats join with `tacit vault join`.
    New {
        name: String,
        /// The three seat keys in seat order, comma separated. `me` stands for
        /// your own. Example: --seats me,3fa2...,91c0...
        #[arg(long, value_delimiter = ',')]
        seats: Vec<String>,
    },
    /// Join key generation in a room someone else opened.
    Join { name: String, room: String },
    /// Vaults you hold a seat in.
    List,
    /// A vault's address, viewing key and balance.
    Show { name: String },
}

pub async fn run(home: &Home, cmd: VaultCmd) -> Result<()> {
    match cmd {
        VaultCmd::New { name, seats } => {
            home.ensure_new_vault(&name)?;
            let me = home.seat_key()?.public();
            let seats = seats
                .iter()
                .map(|s| {
                    if s == "me" {
                        return Ok(me);
                    }
                    hex::decode(s.trim())
                        .ok()
                        .and_then(|b| b.try_into().ok())
                        .ok_or_else(|| anyhow!("{s} is not a seat key (64 hex characters)"))
                })
                .collect::<Result<Vec<SeatPub>>>()?;
            if seats.len() != 3 {
                bail!("a vault has exactly three seats; got {}", seats.len());
            }
            if !seats.contains(&me) {
                bail!("your own seat isn't in the list; add `me` where you sit");
            }
            let room = blocking(home.relay(), move |r| Ok(r.open_room_for(&seats, "dkg")?)).await?;
            println!("room {room}");
            println!("the other two seats run:  tacit vault join <name> {room}");
            keygen(home, &name, &room).await
        }
        VaultCmd::Join { name, room } => {
            home.ensure_new_vault(&name)?;
            keygen(home, &name, &room).await
        }
        VaultCmd::List => {
            for name in home.vault_names()? {
                let v = home.load_vault(&name)?;
                println!("{name}  seat {} of 3  {}", v.seat, v.vault.encode_address(&home.params, 0));
            }
            Ok(())
        }
        VaultCmd::Show { name } => {
            let v = home.load_vault(&name)?;
            let mut client = home.client().await?;
            let (mut wallet, account) = v.wallet(home, &mut client).await?;
            wallet.sync(&mut client).await?;
            println!("vault        {name}");
            println!("your seat    {} of 3", v.seat);
            println!("created at   height {}", v.birthday);
            println!("address      {}", v.vault.encode_address(&home.params, 0));
            println!("viewing key  {}", v.vault.ufvk().encode(&home.params));
            println!("balance      {}", zec(wallet.spendable(account)?));
            Ok(())
        }
    }
}

/// Runs this seat's side of key generation and saves the result.
pub async fn keygen(home: &Home, name: &str, room: &str) -> Result<()> {
    let key = home.seat_key()?;
    let room_id = room.to_string();
    let seats = blocking(home.relay(), move |r| Ok(r.room(&room_id)?)).await?;
    let me = key.public();
    let seat = seats.iter().position(|s| *s == me).ok_or_else(|| anyhow!("you don't have a seat in room {room}"))? + 1;
    // The vault's wallet scans from the block after this one. Taking the tip
    // before key generation keeps every deposit, which needs the finished
    // vault's address, above it.
    let mut client = home.client().await?;
    let created = Wallet::<Net>::tip(&mut client).await?;
    println!("seat {seat} of 3; waiting for the other seats...");

    let (room_id, seats2) = (room.to_string(), seats.clone());
    let key_bytes = key.to_bytes();
    let share = blocking(home.relay(), move |r| {
        Ok(relay::run_dkg(&r, SeatKey::from_bytes(key_bytes), &room_id, seats2, SEAT_TIMEOUT)?)
    })
    .await?;

    home.save_vault(name, &share, &seats, created)?;
    println!("vault {name} ready: all three seats agree on it");
    println!("address  {}", share.vault.encode_address(&home.params, 0));
    Ok(())
}

/// Runs blocking coordinator work off the async runtime.
pub async fn blocking<T: Send + 'static>(relay: Relay, f: impl FnOnce(Relay) -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(move || f(relay)).await?
}
