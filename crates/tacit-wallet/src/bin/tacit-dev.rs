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
//!
//!   tacit-dev auction-e2e
//!       A full sealed-bid auction: three bidders deposit identical bonds
//!       with timelocked bids, the drand round opens them, the vault settles
//!       in one transaction signed by seller and operator, and an auditor
//!       holding nothing but the published terms verifies the result.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use rand_core::{OsRng, RngCore};
use tacit_vault::dkg::{self, seat};
use tacit_vault::frost::keys::dkg::round1;
use tacit_vault::frost::keys::{KeyPackage, PublicKeyPackage};
use tacit_vault::frost::Identifier;
use tacit_vault::Vault;
use tacit_wallet::{
    connect, ironwood_proving_key, prove, regtest, sign_with_usk, sign_with_vault, Wallet,
};
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

fn run_dkg() -> (BTreeMap<Identifier, KeyPackage>, PublicKeyPackage) {
    let seats: Vec<Identifier> = (1..=3).map(seat).collect();
    let others = |me: &Identifier, all: &BTreeMap<Identifier, round1::Package>| {
        all.iter()
            .filter(|(id, _)| *id != me)
            .map(|(id, p)| (*id, p.clone()))
            .collect::<BTreeMap<_, _>>()
    };
    let (mut r1s, mut r1p) = (BTreeMap::new(), BTreeMap::new());
    for id in &seats {
        let (s, p) = dkg::start(*id, OsRng).unwrap();
        r1s.insert(*id, s);
        r1p.insert(*id, p);
    }
    let (mut r2s, mut outbox) = (BTreeMap::new(), BTreeMap::new());
    for id in &seats {
        let (s, out) = dkg::share(r1s.remove(id).unwrap(), &others(id, &r1p)).unwrap();
        r2s.insert(*id, s);
        outbox.insert(*id, out);
    }
    let mut keys = BTreeMap::new();
    let mut public = None;
    for id in &seats {
        let inbox = outbox
            .iter()
            .filter(|(from, _)| *from != id)
            .map(|(from, out)| (*from, out[id].clone()))
            .collect();
        let (kp, pkp) = dkg::finish(&r2s[id], &others(id, &r1p), &inbox).unwrap();
        if let Some(p) = &public {
            assert_eq!(p, &pkp, "all seats must agree on the group key");
        }
        public = Some(pkp);
        keys.insert(*id, kp);
    }
    (keys, public.unwrap())
}

async fn vault_e2e() {
    let params = regtest();
    let seed = faucet_seed();
    let mut client = connect(GRPC).await.expect("Zebra's lightwalletd endpoint is not reachable on 9067");

    let data = regtest_dir().join("e2e");
    let _ = fs::remove_dir_all(&data);
    fs::create_dir_all(&data).unwrap();
    let mut wallet = Wallet::open(data.join("wallet.sqlite"), params.clone()).unwrap();

    let t = step("faucet wallet");
    let birthday = Wallet::<zcash_protocol::local_consensus::LocalNetwork>::birthday_now(&mut client)
        .await
        .unwrap();
    let (faucet, usk) = wallet.create_account("faucet", seed, &birthday).unwrap();
    mine(COINBASE_MATURITY + 2);
    wallet.sync(&mut client).await.unwrap();
    let faucet_funds = wallet.spendable(faucet).unwrap();
    println!("   faucet spendable: {} ZEC ({:?})", faucet_funds as f64 / ZEC as f64, t.elapsed());
    assert!(faucet_funds > 2 * ZEC, "coinbase should have matured into spendable Ironwood notes");

    let t = step("2 of 3 FROST vault via DKG");
    let (keys, public) = run_dkg();
    let vault = Vault::create(public, OsRng).unwrap();
    let vault_birthday = Wallet::<zcash_protocol::local_consensus::LocalNetwork>::birthday_now(&mut client)
        .await
        .unwrap();
    let vault_account = wallet.import_vault("vault", &vault, &vault_birthday).unwrap();
    let vault_addr = vault.encode_address(&params, 0);
    println!("   vault address {}...  ({:?})", &vault_addr[..24], t.elapsed());

    let t = step("proving key");
    let pk = ironwood_proving_key();
    println!("   built in {:?}", t.elapsed());

    let t = step("deposit 1 ZEC from faucet into the vault");
    let pczt = wallet.propose(faucet, &vault_addr, ZEC, None).unwrap();
    let pczt = prove(pczt, &pk).unwrap();
    let pczt = sign_with_usk(pczt, &usk).unwrap();
    let txid = wallet.finish_and_send(&mut client, pczt).await.unwrap();
    println!("   sent {txid} ({:?})", t.elapsed());
    mine(1);
    wallet.sync(&mut client).await.unwrap();
    let in_vault = wallet.spendable(vault_account).unwrap();
    println!("   vault spendable: {} ZEC", in_vault as f64 / ZEC as f64);
    assert_eq!(in_vault, ZEC);

    let t = step("spend 0.5 ZEC out of the vault, signed by seats 1 and 3");
    let back_to = wallet.next_address(faucet).unwrap();
    let before = wallet.spendable(faucet).unwrap();
    let pczt = wallet.propose(vault_account, &back_to, ZEC / 2, None).unwrap();
    let pczt = prove(pczt, &pk).unwrap();
    let signers: BTreeMap<_, _> = [1u16, 3].iter().map(|n| (seat(*n), keys[&seat(*n)].clone())).collect();
    let pczt = sign_with_vault(pczt, &vault, &signers).unwrap();
    let txid = wallet.finish_and_send(&mut client, pczt).await.unwrap();
    println!("   sent {txid} ({:?})", t.elapsed());
    mine(1);
    wallet.sync(&mut client).await.unwrap();

    let vault_left = wallet.spendable(vault_account).unwrap();
    let faucet_gain = wallet.spendable(faucet).unwrap() as i64 - before as i64;
    println!("   vault now holds {} ZEC", vault_left as f64 / ZEC as f64);
    println!("   faucet gained  {} ZEC (plus mining rewards)", faucet_gain as f64 / ZEC as f64);
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
    let mut faucet = Party::new(&data, "faucet", Some(faucet_seed()), &mut client).await;
    mine(COINBASE_MATURITY + 2);
    faucet.sync(&mut client).await;
    println!("   faucet spendable: {} ({:?})", zec(faucet.balance()), t.elapsed());

    let t = step("seller, three bidders, and the vault, each with their own wallet");
    let mut seller = Party::new(&data, "seller", None, &mut client).await;
    let mut bidders = Vec::new();
    for name in ["alice", "bob", "carol"] {
        bidders.push((name, Party::new(&data, name, None, &mut client).await));
    }
    let (keys, public) = run_dkg();
    let vault = Vault::create(public, OsRng).unwrap();
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
    // Seat 1 is the seller, seat 2 the operator, seat 3 the arbiter.
    let signers: BTreeMap<_, _> = [1u16, 2].iter().map(|n| (seat(*n), keys[&seat(*n)].clone())).collect();
    let pczt = sign_with_vault(pczt, &vault, &signers).unwrap();
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
        _ => {
            eprintln!("usage: tacit-dev init <zebra-state-dir> | tacit-dev vault-e2e | tacit-dev auction-e2e");
            std::process::exit(2);
        }
    }
}
