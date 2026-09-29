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

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("init") => init(args.get(2).map(String::as_str).unwrap_or("/tmp/tacit-zebra")),
        Some("vault-e2e") => vault_e2e().await,
        _ => {
            eprintln!("usage: tacit-dev init <zebra-state-dir> | tacit-dev vault-e2e");
            std::process::exit(2);
        }
    }
}
