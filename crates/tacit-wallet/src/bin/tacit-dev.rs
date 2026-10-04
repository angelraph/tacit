//! Developer tool for a local Zebra regtest chain.
//!
//!   tacit-dev init <zebra-state-dir>
//!       Creates the dev faucet seed (regtest/faucet.seed) and writes
//!       regtest/zebrad.toml with the faucet as Zebra's miner address.
//!
//!   tacit-dev vault-e2e
//!       With Zebra running from that config: mines coinbase to the faucet,
//!       creates a 2 of 3 FROST vault, deposits into it, and spends out of it
//!       with seats 1 and 3, checking balances on chain at every step.
//!       The three seats run on separate threads and only talk through a
//!       local coordinator, in sealed envelopes, as separate devices would.
//!
//!   tacit-dev fund <address> <zatoshi>
//!       Pays regtest coins from the dev faucet to any address, then mines a
//!       block so they confirm. For trying the `tacit` CLI by hand.
//!
//!   tacit-dev mine <n>
//!       Mines n regtest blocks.
//!
//!   tacit-dev auction-e2e
//!       A full sealed-bid auction: three bidders deposit identical bonds
//!       with timelocked bids, the drand round opens them, the vault settles
//!       in one transaction signed by seller and operator, and an auditor
//!       holding nothing but the published terms verifies the result.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use std::time::Duration;

use pczt::Pczt;
use rand_core::{OsRng, RngCore};
use tacit_core::envelope::SeatKey;
use tacit_vault::session::{SeatPub, VaultShare};
use tacit_vault::{SigningRequest, Vault};
use tacit_wallet::relay::{self, Relay};
use tacit_wallet::{connect, ironwood_proving_key, prove, regtest, sign_with_usk, Wallet};
use zcash_keys::keys::{UnifiedAddressRequest, UnifiedSpendingKey};
use zcash_protocol::consensus::Parameters;
use zip32::AccountId;

const GRPC: &str = "http://127.0.0.1:9067";
const ZEC: u64 = 100_000_000;
/// Zebra won't let coinbase outputs be spent until they are 100 blocks deep.
const COINBASE_MATURITY: u32 = 100;

fn regtest_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../regtest")
}

fn faucet_seed() -> Vec<u8> {
    let path = regtest_dir().join("faucet.seed");
    if let Ok(hex_seed) = fs::read_to_string(&path) {
        return hex::decode(hex_seed.trim()).expect("faucet.seed is hex");
    }
    let mut seed = vec![0u8; 32];
    OsRng.fill_bytes(&mut seed);
    fs::write(&path, hex::encode(&seed)).expect("write faucet.seed");
    seed
}

fn faucet_address(params: &impl Parameters, seed: &[u8]) -> String {
    let usk = UnifiedSpendingKey::from_seed(params, seed, AccountId::ZERO).expect("valid seed");
    let (ua, _) = usk
        .to_unified_full_viewing_key()
        .default_address(UnifiedAddressRequest::ORCHARD)
        .expect("orchard address");
    ua.encode(params)
}

fn init(state_dir: &str) {
    let params = regtest();
    let seed = faucet_seed();
    let template = fs::read_to_string(regtest_dir().join("zebrad.toml.in")).expect("zebrad.toml.in");
    let config = template
        .replace("MINER_ADDRESS", &faucet_address(&params, &seed))
        .replace("STATE_DIR", state_dir);
    fs::write(regtest_dir().join("zebrad.toml"), config).expect("write zebrad.toml");
    println!("wrote regtest/zebrad.toml (miner address = dev faucet)");
}

fn step(label: &str) -> Instant {
    println!("\n== {label}");
    Instant::now()
}

fn mine(n: u32) {
    let h = tacit_regtest::mine(tacit_regtest::DEFAULT_RPC, n).expect("mining");
    println!("   mined {n} block(s), tip is now {h}");
}

/// How long a seat waits on the others before giving up.
const SEAT_TIMEOUT: Duration = Duration::from_secs(120);

/// The three seats of a vault. Each seat only ever touches its own secrets;
/// everything between them goes through the coordinator sealed.
struct Seats {
    url: String,
    keys: Vec<[u8; 32]>,
    pubs: Vec<SeatPub>,
    shares: Vec<VaultShare>,
}

impl Seats {
    fn vault(&self) -> Vault {
        self.shares[0].vault.clone()
    }
}

/// Starts a coordinator on a free local port and runs key generation with
/// each seat on its own thread.
async fn vault_over_coordinator() -> Seats {
    let state = tacit_coordinator::AppState::in_memory().unwrap();
    let url = tacit_coordinator::spawn("127.0.0.1:0", state).await.unwrap();
    tokio::task::spawn_blocking(move || {
        let keys: Vec<[u8; 32]> = (0..3).map(|_| SeatKey::generate().to_bytes()).collect();
        let pubs: Vec<SeatPub> = keys.iter().map(|k| SeatKey::from_bytes(*k).public()).collect();
        let room = Relay::new(&url).open_room(&pubs).unwrap();
        let threads: Vec<_> = keys
            .iter()
            .map(|k| {
                let (k, url, room, pubs) = (*k, url.clone(), room.clone(), pubs.clone());
                std::thread::spawn(move || {
                    relay::run_dkg(&Relay::new(url), SeatKey::from_bytes(k), &room, pubs, SEAT_TIMEOUT)
                })
            })
            .collect();
        let shares: Vec<VaultShare> = threads.into_iter().map(|t| t.join().unwrap().unwrap()).collect();
        let fvk = shares[0].vault.fvk().to_bytes();
        assert!(shares.iter().all(|s| s.vault.fvk().to_bytes() == fvk), "all seats must hold the same vault");
        println!("   key generation over {url}, room {}...", &room[..8]);
        Seats { url, keys, pubs, shares }
    })
    .await
    .unwrap()
}

/// Signs `pczt` with two seats in a fresh room: `coordinator` hands out the
/// PCZT and aggregates, `cosigner` checks what it is asked to sign first.
async fn sign_over_coordinator(seats: &Seats, pczt: Pczt, coordinator: u16, cosigner: u16) -> Pczt {
    let (url, pubs, vault) = (seats.url.clone(), seats.pubs.clone(), seats.vault());
    let (ck, sk) = (seats.keys[coordinator as usize - 1], seats.keys[cosigner as usize - 1]);
    let c_kp = seats.shares[coordinator as usize - 1].key_package.clone();
    let s_kp = seats.shares[cosigner as usize - 1].key_package.clone();
    tokio::task::spawn_blocking(move || {
        let relay = Relay::new(&url);
        let room = relay.open_room(&pubs).unwrap();
        let signer = {
            let (url, room, pubs) = (url.clone(), room.clone(), pubs.clone());
            let coord_pub = pubs[coordinator as usize - 1];
            std::thread::spawn(move || {
                let approve = |p: &Pczt| match SigningRequest::from_pczt(p) {
                    Ok(req) => {
                        println!(
                            "   seat {cosigner} asked to sign {} spend(s), sighash {}...",
                            req.spends.len(),
                            hex::encode(&req.sighash[..8])
                        );
                        true
                    }
                    Err(e) => {
                        println!("   seat {cosigner} refuses an unreadable PCZT: {e}");
                        false
                    }
                };
                let key = SeatKey::from_bytes(sk);
                relay::run_signer(&Relay::new(url), key, &room, pubs, coord_pub, s_kp, approve, SEAT_TIMEOUT)
            })
        };
        let signed = relay::run_coordinator(
            &relay,
            SeatKey::from_bytes(ck),
            &room,
            pubs,
            vault,
            pczt,
            &[coordinator, cosigner],
            Some(c_kp),
            SEAT_TIMEOUT,
        )
        .unwrap();
        signer.join().unwrap().unwrap();
        signed
    })
    .await
    .unwrap()
}

async fn vault_e2e() {
    let params = regtest();
    let mut client = connect(GRPC).await.expect("Zebra's lightwalletd endpoint is not reachable on 9067");
    let data = regtest_dir().join("e2e");
    let _ = fs::remove_dir_all(&data);
    fs::create_dir_all(&data).unwrap();

    let t = step("faucet wallet");
    let mut faucet = faucet(&mut client).await;
    println!("   faucet spendable: {} ({:?})", zec(faucet.balance()), t.elapsed());

    let t = step("2 of 3 FROST vault via DKG");
    let seats = vault_over_coordinator().await;
    let vault = seats.vault();
    let vault_birthday = Wallet::<zcash_protocol::local_consensus::LocalNetwork>::birthday_now(&mut client)
        .await
        .unwrap();
    let mut vault_wallet = Wallet::open(data.join("vault.sqlite"), params.clone()).unwrap();
    let vault_account = vault_wallet.import_vault("vault", &vault, &vault_birthday).unwrap();
    let vault_addr = vault.encode_address(&params, 0);
    println!("   vault address {}...  ({:?})", &vault_addr[..24], t.elapsed());

    let t = step("proving key");
    let pk = ironwood_proving_key();
    println!("   built in {:?}", t.elapsed());

    let t = step("deposit 1 ZEC from faucet into the vault");
    let pczt = faucet.wallet.propose(faucet.account, &vault_addr, ZEC, None).unwrap();
    let pczt = sign_with_usk(prove(pczt, &pk).unwrap(), faucet.usk.as_ref().unwrap()).unwrap();
    let txid = faucet.wallet.finish_and_send(&mut client, pczt).await.unwrap();
    println!("   sent {txid} ({:?})", t.elapsed());
    mine(1);
    vault_wallet.sync(&mut client).await.unwrap();
    let in_vault = vault_wallet.spendable(vault_account).unwrap();
    println!("   vault spendable: {}", zec(in_vault));
    assert_eq!(in_vault, ZEC);

    let t = step("spend 0.5 ZEC out of the vault, signed by seats 1 and 3");
    let back_to = faucet.wallet.next_address(faucet.account).unwrap();
    let pczt = vault_wallet.propose(vault_account, &back_to, ZEC / 2, None).unwrap();
    let pczt = prove(pczt, &pk).unwrap();
    let pczt = sign_over_coordinator(&seats, pczt, 1, 3).await;
    let txid = vault_wallet.finish_and_send(&mut client, pczt).await.unwrap();
    println!("   sent {txid} ({:?})", t.elapsed());
    mine(1);
    vault_wallet.sync(&mut client).await.unwrap();

    let vault_left = vault_wallet.spendable(vault_account).unwrap();
    println!("   vault now holds {}", zec(vault_left));
    assert!(vault_left < ZEC / 2, "vault paid out 0.5 ZEC plus the fee");
    println!("\nvault e2e passed: a 2 of 3 FROST signature authorized a real Ironwood spend on chain");
}

fn zec(z: u64) -> String {
    format!("{:.4} ZEC", z as f64 / ZEC as f64)
}

/// One participant: their own wallet database, the way it is in real use.
struct Party {
    wallet: Wallet<zcash_protocol::local_consensus::LocalNetwork>,
    account: zcash_client_sqlite::AccountUuid,
    usk: Option<UnifiedSpendingKey>,
}

impl Party {
    async fn new(dir: &Path, name: &str, seed: Option<Vec<u8>>, client: &mut tacit_wallet::GrpcClient) -> Self {
        let mut wallet = Wallet::open(dir.join(format!("{name}.sqlite")), regtest()).unwrap();
        let birthday = Wallet::<zcash_protocol::local_consensus::LocalNetwork>::birthday_now(client).await.unwrap();
        let seed = seed.unwrap_or_else(|| {
            let mut s = vec![0u8; 32];
            OsRng.fill_bytes(&mut s);
            s
        });
        let (account, usk) = wallet.create_account(name, seed, &birthday).unwrap();
        Party { wallet, account, usk: Some(usk) }
    }

    async fn sync(&mut self, client: &mut tacit_wallet::GrpcClient) {
        self.wallet.sync(client).await.unwrap();
    }

    fn balance(&self) -> u64 {
        self.wallet.spendable(self.account).unwrap()
    }
}

/// Spendable balance the faucet tops itself up to before a run.
const FAUCET_FLOOR: u64 = 50 * ZEC;

/// The dev faucet, kept in regtest/faucet/ between runs so that only the
/// first run on a fresh chain pays for 100 blocks of coinbase maturity.
///
/// Zebra pays every mined block to the faucet, so once it exists it keeps
/// growing on its own. If Zebra's chain was reset (the wallet has synced past
/// the node's tip) the faucet wallet is thrown away and rebuilt.
async fn faucet(client: &mut tacit_wallet::GrpcClient) -> Party {
    let dir = regtest_dir().join("faucet");
    let db = dir.join("faucet.sqlite");
    let tip = tacit_regtest::height(tacit_regtest::DEFAULT_RPC).expect("Zebra RPC is not reachable on 18232") as u32;

    let existing = if db.exists() {
        let wallet = Wallet::open(&db, regtest()).unwrap();
        match (wallet.synced_height().unwrap(), wallet.accounts().unwrap().first().copied()) {
            (Some(h), Some(account)) if h <= tip => Some((wallet, account)),
            _ => {
                println!("   faucet wallet doesn't match this chain; rebuilding it");
                drop(wallet);
                fs::remove_dir_all(&dir).unwrap();
                None
            }
        }
    } else {
        None
    };

    let seed = faucet_seed();
    let mut party = match existing {
        Some((wallet, account)) => {
            let usk = UnifiedSpendingKey::from_seed(&regtest(), &seed, AccountId::ZERO).unwrap();
            Party { wallet, account, usk: Some(usk) }
        }
        None => {
            fs::create_dir_all(&dir).unwrap();
            Party::new(&dir, "faucet", Some(seed), client).await
        }
    };
    party.sync(client).await;
    if party.balance() < FAUCET_FLOOR {
        mine(COINBASE_MATURITY + 2);
        party.sync(client).await;
    }
    assert!(party.balance() >= FAUCET_FLOOR, "faucet should hold matured coinbase by now");
    party
}

async fn fund(to: &str, zat: u64) {
    let mut client = connect(GRPC).await.expect("Zebra's lightwalletd endpoint is not reachable on 9067");
    let mut faucet = faucet(&mut client).await;
    let pczt = faucet.wallet.propose(faucet.account, to, zat, None).unwrap();
    let pczt = sign_with_usk(prove(pczt, &ironwood_proving_key()).unwrap(), faucet.usk.as_ref().unwrap()).unwrap();
    let txid = faucet.wallet.finish_and_send(&mut client, pczt).await.unwrap();
    mine(1);
    println!("sent {} to {to} in {txid}", zec(zat));
}

async fn auction_e2e() {
    use tacit_core::auction::{self, PriceRule, Terms};
    use tacit_core::seal;
    use tacit_wallet::auction::{deposits, place_bid, settlement_payments, verify};
    use tacit_wallet::drand;
    use std::time::{SystemTime, UNIX_EPOCH};

    type Net = zcash_protocol::local_consensus::LocalNetwork;
    let params = regtest();
    let mut client = connect(GRPC).await.expect("Zebra's lightwalletd endpoint is not reachable on 9067");
    let data = regtest_dir().join("auction");
    let _ = fs::remove_dir_all(&data);
    fs::create_dir_all(&data).unwrap();
    let pk = ironwood_proving_key();

    let t = step("faucet");
    let mut faucet = faucet(&mut client).await;
    println!("   faucet spendable: {} ({:?})", zec(faucet.balance()), t.elapsed());

    let t = step("seller, three bidders, and the vault, each with their own wallet");
    let mut seller = Party::new(&data, "seller", None, &mut client).await;
    let mut bidders = Vec::new();
    for name in ["alice", "bob", "carol"] {
        bidders.push((name, Party::new(&data, name, None, &mut client).await));
    }
    let seats = vault_over_coordinator().await;
    let vault = seats.vault();
    let created = Wallet::<Net>::birthday_now(&mut client).await.unwrap();
    let mut vault_wallet = Wallet::open(data.join("vault.sqlite"), params.clone()).unwrap();
    let vault_account = vault_wallet.import_vault("vault", &vault, &created).unwrap();
    let vault_addr = vault.encode_address(&params, 0);
    let seller_addr = seller.wallet.next_address(seller.account).unwrap();

    let bond = ZEC;
    let mut funding = Vec::new();
    for (_, b) in bidders.iter_mut() {
        funding.push((b.wallet.next_address(b.account).unwrap(), bond + ZEC / 100, None));
    }
    let pczt = faucet.wallet.propose_payments(faucet.account, &funding).unwrap();
    let pczt = sign_with_usk(prove(pczt, &pk).unwrap(), faucet.usk.as_ref().unwrap()).unwrap();
    faucet.wallet.finish_and_send(&mut client, pczt).await.unwrap();
    mine(1);
    for (_, b) in bidders.iter_mut() {
        b.sync(&mut client).await;
        assert_eq!(b.balance(), bond + ZEC / 100);
    }
    println!("   each bidder funded with {} ({:?})", zec(bond + ZEC / 100), t.elapsed());

    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let tip = tacit_regtest::height(tacit_regtest::DEFAULT_RPC).unwrap() as u32;
    let terms = Terms {
        network: "regtest".into(),
        vault_ufvk: vault.ufvk().encode(&params),
        seller: seller_addr.clone(),
        item: "Signed first edition, lot 7".into(),
        created_height: u32::try_from(created.height()).unwrap(),
        reserve: ZEC / 10,
        bond,
        bids_until_height: tip + 10,
        close_round: seal::round_at(now + 120),
        rule: PriceRule::SecondPrice,
    };
    terms.validate().unwrap();
    println!("\n   auction {}", hex::encode(&terms.id()[..8]));
    println!("   second price, reserve {}, bond {}, bids sealed to drand round {}",
        zec(terms.reserve), zec(terms.bond), terms.close_round);

    let t = step("sealed bids");
    let amounts = [35 * ZEC / 100, 80 * ZEC / 100, 60 * ZEC / 100];
    for ((name, b), amount) in bidders.iter_mut().zip(amounts) {
        let usk = b.usk.clone().unwrap();
        let txid = place_bid(&mut b.wallet, &mut client, &pk, b.account, &usk, &terms, &vault_addr, amount)
            .await
            .unwrap();
        println!("   {name} bid (sealed) in {}", &txid.to_string()[..16]);
    }
    mine(1);
    vault_wallet.sync(&mut client).await.unwrap();
    let deps = deposits(&vault_wallet, vault_account).unwrap();
    println!("   vault holds {} deposits of {} each ({:?})", deps.len(), zec(bond), t.elapsed());
    assert_eq!(deps.len(), 3);
    assert!(deps.iter().all(|d| d.value == bond), "bonds are identical, so they reveal nothing");
    assert!(deps.iter().all(|d| !d.memo.is_empty()), "the vault must have the full memos");
    assert!(drand::fetch(terms.close_round).is_none(), "the closing round must still be in the future");
    println!("   drand round {} not yet published: nobody can read a bid, including us", terms.close_round);

    let t = step("waiting for the closing round");
    mine(terms.bids_until_height - tip);
    let beacon = tokio::task::spawn_blocking({
        let round = terms.close_round;
        move || drand::wait_for(round)
    })
    .await
    .unwrap();
    println!("   round {} published and verified ({:?})", beacon.round(), t.elapsed());

    let t = step("settlement");
    vault_wallet.sync(&mut client).await.unwrap();
    let deps = deposits(&vault_wallet, vault_account).unwrap();
    let outcome = auction::settle(&terms, &deps, &beacon).unwrap();
    for d in &outcome.opened {
        println!("   deposit {}: bid {}", hex::encode(&d.deposit_id[..6]), d.bid.map(zec).unwrap_or("-".into()));
    }
    let price = outcome.price.expect("three valid bids were placed, so there must be a winner");
    println!("   clearing price {}, fee {}", zec(price), zec(outcome.fee));
    let payments: Vec<_> = settlement_payments(&params, &terms, &outcome)
        .into_iter()
        .map(|(to, amount)| (to, amount, None))
        .collect();
    let pczt = vault_wallet.propose_payments(vault_account, &payments).unwrap();
    let pczt = prove(pczt, &pk).unwrap();
    // Seat 1 is the seller, seat 2 the operator, seat 3 the arbiter. The
    // operator proved the settlement, so it coordinates; the seller cosigns.
    let pczt = sign_over_coordinator(&seats, pczt, 2, 1).await;
    let txid = vault_wallet.finish_and_send(&mut client, pczt).await.unwrap();
    mine(1);
    println!("   settled in {} ({:?})", &txid.to_string()[..16], t.elapsed());

    seller.sync(&mut client).await;
    println!("   seller now has {}", zec(seller.balance()));
    assert_eq!(seller.balance(), price - outcome.fee);
    for (name, b) in bidders.iter_mut() {
        b.sync(&mut client).await;
        println!("   {name:<6} now has {}", zec(b.balance()));
    }
    vault_wallet.sync(&mut client).await.unwrap();
    assert_eq!(vault_wallet.spendable(vault_account).unwrap(), 0, "the vault is emptied by settlement");

    let t = step("independent audit from the published terms only");
    let verdict = verify(params.clone(), &mut client, &terms, &beacon, &data.join("auditor.sqlite"))
        .await
        .unwrap();
    println!("   settlement tx found: {}", verdict.settlement.map(|t| t.to_string()[..16].to_string()).unwrap_or("none".into()));
    println!("   missing payments: {}, unexpected payments: {} ({:?})", verdict.missing.len(), verdict.unexpected.len(), t.elapsed());
    assert!(verdict.is_honest(), "{verdict:?}");
    println!("\nauction e2e passed: sealed bids, public clearing price, no identities, settlement verified by an outsider");
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("init") => init(args.get(2).map(String::as_str).unwrap_or("/tmp/tacit-zebra")),
        Some("vault-e2e") => vault_e2e().await,
        Some("auction-e2e") => auction_e2e().await,
        Some("mine") => mine(args.get(2).and_then(|n| n.parse().ok()).unwrap_or(1)),
        Some("fund") => {
            let (Some(to), Some(zat)) = (args.get(2), args.get(3).and_then(|z| z.parse::<u64>().ok())) else {
                eprintln!("usage: tacit-dev fund <address> <zatoshi>");
                std::process::exit(2);
            };
            fund(to, zat).await
        }
        _ => {
            eprintln!("usage: tacit-dev init <zebra-state-dir> | vault-e2e | auction-e2e | fund <address> <zatoshi> | mine <n>");
            std::process::exit(2);
        }
    }
}
