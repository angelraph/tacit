//! `tacit`: a shielded wallet, FROST vault seats, and sealed-bid auctions on
//! Zcash, from the command line.
//!
//! Every participant runs their own `tacit` with their own home directory.
//! Vault seats only talk to each other through a coordinator, and only in
//! sealed envelopes; the coordinator can't read or forge anything.

mod agent;
mod faucet;
mod home;
mod market;
mod net;
mod vault;

use std::io::Write as _;
use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use rand_core::{OsRng, RngCore};
use tacit_wallet::{ironwood_proving_key, prove, sign_with_usk, Wallet};

use home::{Config, Home};
use net::{Net, NetName};

#[derive(Parser)]
#[command(name = "tacit", version, about = "Sealed-bid markets and shielded escrow on Zcash")]
struct Cli {
    /// regtest, test or main.
    #[arg(long, global = true, env = "TACIT_NETWORK", default_value = "regtest")]
    network: NetName,
    /// Where this user's keys and wallets live [default: ~/.tacit/<network>].
    #[arg(long, global = true, env = "TACIT_HOME")]
    home: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a wallet, a seat key and a config in the home directory.
    Init {
        /// Light wallet (lightwalletd gRPC) server. Regtest defaults to the local Zebra.
        #[arg(long)]
        server: Option<String>,
        /// Tacit coordinator.
        #[arg(long, default_value = "http://127.0.0.1:8650")]
        coordinator: String,
    },
    /// Print a fresh shielded address for receiving.
    Address,
    /// Sync and print the spendable balance.
    Balance,
    /// Send ZEC from the personal wallet.
    Send {
        to: String,
        /// Amount in ZEC, e.g. 0.25.
        amount: String,
        /// Text memo for the recipient.
        #[arg(long)]
        memo: Option<String>,
    },
    /// Print this user's seat key, which co-seats need to open a vault with you.
    Seat,
    /// FROST 2 of 3 vaults.
    #[command(subcommand)]
    Vault(vault::VaultCmd),
    /// Sealed-bid auctions.
    #[command(subcommand)]
    Auction(market::AuctionCmd),
    /// Run as an always-online operator or arbiter seat: join key generation,
    /// settle auctions when they unseal, and cosign what the rules allow.
    Agent {
        #[arg(long, value_enum)]
        role: agent::Role,
        /// Name shown to sellers choosing seats.
        #[arg(long, default_value = "Tacit agent")]
        name: String,
    },
    /// Run a testnet faucet that drips this home's coins to anyone, at most
    /// once a day per address and per IP. The coordinator relays it.
    Faucet {
        #[arg(long, default_value = "127.0.0.1:8091")]
        listen: String,
        /// TAZ per drip.
        #[arg(long, default_value = "0.05")]
        amount: String,
    },
    /// As an arbiter, rule for the requester of an escrow dispute by
    /// cosigning the payout they asked for in `room`.
    Decide { room: String },
    /// Seal a bid and deposit the bond for an auction.
    Bid {
        auction: String,
        /// Bid in ZEC; at least the reserve, at most the bond.
        amount: String,
    },
}

pub const ZAT_PER_ZEC: u64 = 100_000_000;

/// Parses a decimal ZEC amount into zatoshi, exactly.
pub fn parse_zec(s: &str) -> Result<u64> {
    let (whole, frac) = s.split_once('.').unwrap_or((s, ""));
    if frac.len() > 8 || (whole.is_empty() && frac.is_empty()) {
        bail!("{s} is not a ZEC amount (at most 8 decimal places)");
    }
    let whole: u64 = if whole.is_empty() { 0 } else { whole.parse().map_err(|_| anyhow!("{s} is not a ZEC amount"))? };
    let frac: u64 = if frac.is_empty() { 0 } else { format!("{frac:0<8}").parse().map_err(|_| anyhow!("{s} is not a ZEC amount"))? };
    whole
        .checked_mul(ZAT_PER_ZEC)
        .and_then(|w| w.checked_add(frac))
        .ok_or_else(|| anyhow!("{s} is too large"))
}

pub fn zec(zat: u64) -> String {
    let s = format!("{}.{:08}", zat / ZAT_PER_ZEC, zat % ZAT_PER_ZEC);
    let s = s.trim_end_matches('0');
    format!("{} ZEC", s.strip_suffix('.').map(|w| format!("{w}.0")).unwrap_or(s.to_string()))
}

/// Asks a yes/no question on the terminal. Anything but y or yes is no.
pub fn confirm(question: &str) -> bool {
    print!("{question} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).is_ok() && matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
}

/// A ZIP 302 text memo.
fn text_memo(text: &str) -> Result<[u8; 512]> {
    let b = text.as_bytes();
    if b.len() > 512 {
        bail!("memo is {} bytes; the limit is 512", b.len());
    }
    let mut memo = [0u8; 512];
    memo[..b.len()].copy_from_slice(b);
    Ok(memo)
}

async fn init(dir: PathBuf, net: NetName, server: Option<String>, coordinator: String) -> Result<()> {
    let server = server
        .or_else(|| net.default_server().map(str::to_string))
        .ok_or_else(|| anyhow!("pass --server with a light wallet server for {net}"))?;
    let mut client = tacit_wallet::connect(&server)
        .await
        .with_context(|| format!("can't reach the light wallet server at {server}"))?;
    let birthday = Wallet::<Net>::birthday_now(&mut client).await?;
    let mut seed = vec![0u8; 32];
    OsRng.fill_bytes(&mut seed);
    let cfg = Config {
        network: net.to_string(),
        server,
        coordinator,
        birthday: u32::from(birthday.height()),
    };
    let home = Home::create(dir, net, cfg, &seed)?;
    let mut wallet = home.wallet()?;
    let (account, _) = wallet.create_account("personal", seed, &birthday)?;
    println!("created a {net} wallet in {}", home.dir.display());
    println!("  birthday height  {}", home.cfg.birthday);
    println!("  address          {}", wallet.next_address(account)?);
    println!("  seat key         {}", hex::encode(home.seat_key()?.public()));
    println!("\nThe seed is stored unencrypted in {}. Fine for testing, not for real funds.", home.dir.join("seed").display());
    Ok(())
}

async fn balance(home: &Home) -> Result<()> {
    let mut client = home.client().await?;
    let (mut wallet, account) = home.personal()?;
    wallet.sync(&mut client).await?;
    println!("{}", zec(wallet.spendable(account)?));
    Ok(())
}

async fn send(home: &Home, to: &str, amount: &str, memo: Option<String>) -> Result<()> {
    let amount = parse_zec(amount)?;
    let memo = memo.as_deref().map(text_memo).transpose()?;
    let mut client = home.client().await?;
    let (mut wallet, account) = home.personal()?;
    wallet.sync(&mut client).await?;
    let pczt = wallet.propose(account, to, amount, memo)?;
    println!("proving...");
    let pczt = sign_with_usk(prove(pczt, &ironwood_proving_key())?, &home.usk()?)?;
    let txid = wallet.finish_and_send(&mut client, pczt).await?;
    println!("sent {} to {to}\ntxid {txid}", zec(amount));
    Ok(())
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli).await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    let dir = cli.home.clone().unwrap_or_else(|| home::default_dir(cli.network));
    if let Cmd::Init { server, coordinator } = cli.cmd {
        return init(dir, cli.network, server, coordinator).await;
    }
    let home = Home::open(dir, cli.network)?;
    match cli.cmd {
        Cmd::Init { .. } => unreachable!(),
        Cmd::Address => {
            let (mut wallet, account) = home.personal()?;
            println!("{}", wallet.next_address(account)?);
            Ok(())
        }
        Cmd::Balance => balance(&home).await,
        Cmd::Send { to, amount, memo } => send(&home, &to, &amount, memo).await,
        Cmd::Seat => {
            println!("{}", hex::encode(home.seat_key()?.public()));
            Ok(())
        }
        Cmd::Vault(c) => vault::run(&home, c).await,
        Cmd::Auction(c) => market::run(&home, c).await,
        Cmd::Bid { auction, amount } => market::bid(&home, &auction, &amount).await,
        Cmd::Agent { role, name } => agent::run(&home, role, name).await,
        Cmd::Decide { room } => agent::decide(&home, &room).await,
        Cmd::Faucet { listen, amount } => faucet::run(&home, listen, amount).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zec_amounts_parse_exactly() {
        assert_eq!(parse_zec("1").unwrap(), ZAT_PER_ZEC);
        assert_eq!(parse_zec("0.6").unwrap(), 60_000_000);
        assert_eq!(parse_zec(".00000001").unwrap(), 1);
        assert_eq!(parse_zec("12.34567891").unwrap(), 1_234_567_891);
        assert!(parse_zec("0.000000001").is_err());
        assert!(parse_zec("1,5").is_err());
        assert!(parse_zec("").is_err());
        assert!(parse_zec("-1").is_err());
    }

    #[test]
    fn zec_amounts_print_without_noise() {
        assert_eq!(zec(ZAT_PER_ZEC), "1.0 ZEC");
        assert_eq!(zec(60_000_000), "0.6 ZEC");
        assert_eq!(zec(1), "0.00000001 ZEC");
        assert_eq!(zec(0), "0.0 ZEC");
    }
}
