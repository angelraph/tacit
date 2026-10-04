//! Tacit's wallet layer.
//!
//! Wraps librustzcash's wallet backend so that ordinary wallets and FROST
//! vaults go through one pipeline:
//!
//! ```text
//! propose -> PCZT -> Ironwood proof -> signatures -> extract -> broadcast
//! ```
//!
//! The only thing that differs between a personal wallet and a vault is who
//! produces the spend authorization signatures.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::path::Path;

use orchard::circuit::{OrchardCircuitVersion, ProvingKey};
use orchard::keys::SpendAuthorizingKey;
use pczt::roles::prover::Prover;
use pczt::roles::signer::Signer;
use pczt::Pczt;
use rand_core::OsRng;
use secrecy::SecretVec;
use tonic::transport::Channel;
use zcash_client_backend::data_api::wallet::{
    create_pczt_from_proposal, extract_and_store_transaction_from_pczt,
    input_selection::{GreedyInputSelector, SpendPolicy},
    propose_transfer, ConfirmationsPolicy,
};
use zcash_client_backend::fees::zip317::SingleOutputChangeStrategy;
use zcash_client_backend::fees::DustOutputPolicy;
use zip321::{Payment, TransactionRequest};
use zcash_client_backend::data_api::{
    Account as _, AccountBirthday, AccountPurpose, WalletRead, WalletWrite,
};
use zcash_client_backend::fees::StandardFeeRule;
use zcash_client_backend::proto::service::compact_tx_streamer_client::CompactTxStreamerClient;
use zcash_client_backend::proto::service::{BlockId, ChainSpec, RawTransaction};
use zcash_client_backend::wallet::OvkPolicy;
use zcash_client_sqlite::util::SystemClock;
use zcash_client_sqlite::wallet::init::init_wallet_db;
use zcash_client_sqlite::{AccountUuid, WalletDb};
use zcash_keys::address::Address;
use zcash_keys::keys::{UnifiedAddressRequest, UnifiedFullViewingKey, UnifiedSpendingKey};
use zcash_primitives::transaction::builder::BundlePadding;
use zcash_primitives::transaction::TxId;
use zcash_protocol::consensus::{BlockHeight, Parameters};
use zcash_protocol::local_consensus::LocalNetwork;
use zcash_protocol::memo::MemoBytes;
use zcash_protocol::value::Zatoshis;
use zcash_protocol::ShieldedPool;

use tacit_vault::frost::keys::KeyPackage;
use tacit_vault::frost::Identifier;
use tacit_vault::Vault;

pub mod auction;
pub mod cache;
pub mod drand;
use cache::MemBlockCache;

pub type Db<P> = WalletDb<rusqlite::Connection, P, SystemClock, OsRng>;
pub type GrpcClient = CompactTxStreamerClient<Channel>;

/// A note an account received.
#[derive(Clone, Debug)]
pub struct Received {
    /// txid followed by the action index (little endian u32); unique and public.
    pub id: Vec<u8>,
    pub txid: TxId,
    pub value: u64,
    pub mined_height: u32,
    pub memo: Vec<u8>,
}

/// The raw 43 byte Orchard-protocol receiver inside a unified address, which
/// is what sealed bids carry for refunds.
pub fn raw_receiver(params: &impl Parameters, address: &str) -> Option<[u8; 43]> {
    match Address::decode(params, address)? {
        Address::Unified(ua) => ua.orchard().map(|o| o.to_raw_address_bytes()),
        _ => None,
    }
}

/// Encodes a raw receiver as a unified address on `params`' network.
pub fn address_from_raw(params: &impl Parameters, raw: &[u8; 43]) -> String {
    use zcash_address::unified::{self, Encoding};
    use zcash_address::ToAddress;
    let ua = unified::Address::try_from_items(vec![unified::Receiver::Orchard(*raw)])
        .expect("a single Orchard receiver is a valid unified address");
    zcash_address::ZcashAddress::from_unified(params.network_type(), ua).encode()
}

#[derive(Debug, thiserror::Error)]
pub enum WalletError {
    #[error("wallet database: {0}")]
    Db(String),
    #[error("light wallet server: {0}")]
    Grpc(String),
    #[error("sync: {0}")]
    Sync(String),
    #[error("building transaction: {0}")]
    Build(String),
    #[error("proving: {0}")]
    Prove(String),
    #[error("signing: {0}")]
    Sign(String),
    #[error("vault: {0}")]
    Vault(#[from] tacit_vault::VaultError),
}

fn db_err(e: impl std::fmt::Debug) -> WalletError {
    WalletError::Db(format!("{e:?}"))
}

/// Parameters for a local regtest chain with every upgrade, NU6.3 included,
/// active from height 1. Matches Zebra's regtest defaults.
pub fn regtest() -> LocalNetwork {
    let h = Some(BlockHeight::from_u32(1));
    LocalNetwork {
        overwinter: h,
        sapling: h,
        blossom: h,
        heartwood: h,
        canopy: h,
        nu5: h,
        nu6: h,
        nu6_1: h,
        nu6_2: h,
        nu6_3: h,
    }
}

pub async fn connect(url: &str) -> Result<CompactTxStreamerClient<Channel>, WalletError> {
    CompactTxStreamerClient::connect(url.to_string())
        .await
        .map_err(|e| WalletError::Grpc(e.to_string()))
}

/// Proving key for the post NU6.3 circuit Ironwood uses. Building it takes a
/// few seconds, so callers should keep one around.
pub fn ironwood_proving_key() -> ProvingKey {
    ProvingKey::build(OrchardCircuitVersion::PostNu6_3)
}

pub struct Wallet<P: Parameters + Clone + Send + 'static> {
    db: Db<P>,
    path: std::path::PathBuf,
    params: P,
    cache: MemBlockCache,
}

impl<P: Parameters + Clone + Send + Sync + 'static> Wallet<P> {
    pub fn open(path: impl AsRef<Path>, params: P) -> Result<Self, WalletError> {
        let path = path.as_ref().to_path_buf();
        let mut db = WalletDb::for_path(&path, params.clone(), SystemClock, OsRng).map_err(db_err)?;
        init_wallet_db(&mut db, None).map_err(db_err)?;
        Ok(Self { db, path, params, cache: MemBlockCache::new() })
    }

    pub fn params(&self) -> &P {
        &self.params
    }

    /// Birthday at the server's current tip, for accounts created now.
    pub async fn birthday_now(
        client: &mut CompactTxStreamerClient<Channel>,
    ) -> Result<AccountBirthday, WalletError> {
        let tip = client
            .get_latest_block(ChainSpec::default())
            .await
            .map_err(|e| WalletError::Grpc(e.to_string()))?
            .into_inner();
        // Anchor at the tip: the account scans from the next block on. (Asking
        // for height 0 doesn't work anyway; protobuf can't tell it from "unset".)
        let height = tip.height;
        let ts = client
            .get_tree_state(BlockId { height, hash: vec![] })
            .await
            .map_err(|e| WalletError::Grpc(e.to_string()))?
            .into_inner();
        AccountBirthday::from_treestate(ts, None).map_err(|_| WalletError::Grpc("bad tree state".into()))
    }

    /// Creates a personal account from a seed. Returns its id and spending key.
    pub fn create_account(
        &mut self,
        name: &str,
        seed: Vec<u8>,
        birthday: &AccountBirthday,
    ) -> Result<(AccountUuid, UnifiedSpendingKey), WalletError> {
        self.db
            .create_account(name, &SecretVec::new(seed), birthday, None)
            .map_err(db_err)
    }

    /// Tracks a vault. The wallet sees its funds and can build transactions
    /// spending them, but holds no spending key; signatures come from FROST.
    pub fn import_vault(
        &mut self,
        name: &str,
        vault: &Vault,
        birthday: &AccountBirthday,
    ) -> Result<AccountUuid, WalletError> {
        self.import_ufvk(name, &vault.ufvk(), birthday, AccountPurpose::Spending { derivation: None })
    }

    /// Tracks any account by its viewing key.
    pub fn import_ufvk(
        &mut self,
        name: &str,
        ufvk: &UnifiedFullViewingKey,
        birthday: &AccountBirthday,
        purpose: AccountPurpose,
    ) -> Result<AccountUuid, WalletError> {
        let account = self.db.import_account_ufvk(name, ufvk, birthday, purpose, None).map_err(db_err)?;
        Ok(account.id())
    }

    /// Every account in this wallet, oldest first.
    pub fn accounts(&self) -> Result<Vec<AccountUuid>, WalletError> {
        self.db.get_account_ids().map_err(db_err)
    }

    /// Height of the chain tip this wallet has synced to, if it has synced.
    pub fn synced_height(&self) -> Result<Option<u32>, WalletError> {
        Ok(self.db.chain_height().map_err(db_err)?.map(u32::from))
    }

    /// A fresh shielded receiving address for `account`.
    pub fn next_address(&mut self, account: AccountUuid) -> Result<String, WalletError> {
        let (ua, _) = self
            .db
            .get_next_available_address(account, UnifiedAddressRequest::ORCHARD)
            .map_err(db_err)?
            .ok_or_else(|| WalletError::Db("account has no address".into()))?;
        Ok(ua.encode(&self.params))
    }

    /// Spendable shielded balance of `account`, in zatoshi.
    pub fn spendable(&self, account: AccountUuid) -> Result<u64, WalletError> {
        let summary = self
            .db
            .get_wallet_summary(ConfirmationsPolicy::MIN)
            .map_err(db_err)?;
        Ok(summary
            .and_then(|s| s.account_balances().get(&account).map(|b| b.spendable_value().into_u64()))
            .unwrap_or(0))
    }

    pub async fn sync(
        &mut self,
        client: &mut CompactTxStreamerClient<Channel>,
    ) -> Result<(), WalletError> {
        zcash_client_backend::sync::run(client, &self.params, &self.cache, &mut self.db, 1000)
            .await
            .map_err(|e| WalletError::Sync(format!("{e:?}")))?;
        self.enhance(client).await
    }

    /// Compact blocks carry only the start of each note's ciphertext, which
    /// is enough to find our notes and their values but not their memos. For
    /// every transaction the wallet asks about, fetch the full transaction so
    /// memos (sealed bids among them) and outgoing payments get stored.
    async fn enhance(&mut self, client: &mut CompactTxStreamerClient<Channel>) -> Result<(), WalletError> {
        use zcash_client_backend::data_api::wallet::decrypt_and_store_transaction;
        use zcash_client_backend::data_api::{TransactionDataRequest, TransactionStatus};
        use zcash_client_backend::proto::service::TxFilter;
        use zcash_primitives::transaction::Transaction;
        use zcash_protocol::consensus::BranchId;

        // Each pass can uncover more work (a fetched transaction may spend
        // notes that point at others), so repeat until nothing new is asked.
        let mut done = std::collections::HashSet::new();
        loop {
            let requests = self.db.transaction_data_requests().map_err(db_err)?;
            let mut progressed = false;
            for request in requests {
                let (txid, wants_data) = match request {
                    TransactionDataRequest::Enhancement(txid) => (txid, true),
                    TransactionDataRequest::GetStatus(txid) => (txid, false),
                    // Tacit never uses transparent addresses.
                    _ => continue,
                };
                if !done.insert((txid, wants_data)) {
                    continue;
                }
                progressed = true;
                let raw = client
                    .get_transaction(TxFilter { block: None, index: 0, hash: txid.as_ref().to_vec() })
                    .await;
                let raw = match raw {
                    Ok(r) => r.into_inner(),
                    Err(status) if status.code() == tonic::Code::NotFound => {
                        self.db
                            .set_transaction_status(txid, TransactionStatus::TxidNotRecognized)
                            .map_err(db_err)?;
                        continue;
                    }
                    Err(e) => return Err(WalletError::Grpc(e.to_string())),
                };
                // lightwalletd reports 0 for mempool transactions and u64::MAX
                // for ones that fell out of the main chain.
                let mined = match raw.height {
                    0 => None,
                    u64::MAX => {
                        self.db
                            .set_transaction_status(txid, TransactionStatus::NotInMainChain)
                            .map_err(db_err)?;
                        continue;
                    }
                    h => Some(BlockHeight::from_u32(h as u32)),
                };
                if wants_data {
                    let branch = BranchId::for_height(&self.params, mined.unwrap_or_else(|| {
                        BlockHeight::from_u32(u32::MAX)
                    }));
                    let tx = Transaction::read(&raw.data[..], branch)
                        .map_err(|e| WalletError::Sync(format!("bad transaction {txid}: {e}")))?;
                    decrypt_and_store_transaction(&self.params, &mut self.db, &tx, mined).map_err(db_err)?;
                } else if let Some(h) = mined {
                    self.db
                        .set_transaction_status(txid, TransactionStatus::Mined(h))
                        .map_err(db_err)?;
                }
            }
            if !progressed {
                return Ok(());
            }
        }
    }

    /// Builds an unsigned, unproven PCZT paying `amount` from `account` to `to`.
    pub fn propose(
        &mut self,
        account: AccountUuid,
        to: &str,
        amount: u64,
        memo: Option<[u8; 512]>,
    ) -> Result<Pczt, WalletError> {
        self.propose_payments(account, &[(to.to_string(), amount, memo)])
    }

    /// Builds an unsigned, unproven PCZT paying several recipients at once.
    /// Change, if any, goes back to `account` in Ironwood.
    pub fn propose_payments(
        &mut self,
        account: AccountUuid,
        payments: &[(String, u64, Option<[u8; 512]>)],
    ) -> Result<Pczt, WalletError> {
        let payments = payments
            .iter()
            .map(|(to, amount, memo)| {
                let addr = Address::decode(&self.params, to)
                    .ok_or_else(|| WalletError::Build(format!("not an address on this network: {to}")))?;
                let amount = Zatoshis::from_u64(*amount).map_err(|_| WalletError::Build("bad amount".into()))?;
                let memo = memo.map(|m| MemoBytes::from_bytes(&m).expect("512 bytes is a valid memo"));
                Payment::new(addr.to_zcash_address(&self.params), Some(amount), memo, None, None, vec![])
                    .map_err(|e| WalletError::Build(format!("{e:?}")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let request = TransactionRequest::new(payments).map_err(|e| WalletError::Build(format!("{e:?}")))?;

        let input_selector = GreedyInputSelector::<Db<P>>::new();
        let change_strategy = SingleOutputChangeStrategy::<StandardFeeRule, Db<P>>::new(
            StandardFeeRule::Zip317,
            None,
            ShieldedPool::Ironwood,
            DustOutputPolicy::default(),
        );
        let proposal = propose_transfer::<_, _, _, _, Infallible>(
            &mut self.db,
            &self.params,
            account,
            &input_selector,
            &change_strategy,
            request,
            ConfirmationsPolicy::MIN,
            &SpendPolicy::default(),
            None,
            None,
        )
        .map_err(|e| WalletError::Build(format!("{e:?}")))?;
        create_pczt_from_proposal::<_, _, Infallible, _, Infallible, _>(
            &mut self.db,
            &self.params,
            account,
            OvkPolicy::Sender,
            &proposal,
            None,
            BundlePadding::DEFAULT,
        )
        .map_err(|e| WalletError::Build(format!("{e:?}")))
    }

    fn read_only(&self) -> Result<rusqlite::Connection, WalletError> {
        rusqlite::Connection::open_with_flags(&self.path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(db_err)
    }

    /// Ironwood notes `account` has received from others (not change), with
    /// their memos and mined heights. For a vault these are the deposits.
    pub fn received(&self, account: AccountUuid) -> Result<Vec<Received>, WalletError> {
        let conn = self.read_only()?;
        let mut stmt = conn
            .prepare(
                "SELECT t.txid, r.output_index, r.value, t.mined_height, r.memo
                 FROM v_received_outputs r
                 JOIN transactions t ON t.id_tx = r.transaction_id
                 JOIN accounts a ON a.id = r.account_id
                 WHERE a.uuid = ?1 AND r.pool = 4 AND r.is_change = 0
                   AND t.mined_height IS NOT NULL
                 ORDER BY t.mined_height, t.txid, r.output_index",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([account.expose_uuid().as_bytes().to_vec()], |row| {
                let txid: Vec<u8> = row.get(0)?;
                let index: u32 = row.get(1)?;
                let mut id = txid.clone();
                id.extend_from_slice(&index.to_le_bytes());
                Ok(Received {
                    id,
                    txid: TxId::from_bytes(txid.try_into().unwrap_or([0; 32])),
                    value: row.get::<_, i64>(2)? as u64,
                    mined_height: row.get(3)?,
                    memo: row.get::<_, Option<Vec<u8>>>(4)?.unwrap_or_default(),
                })
            })
            .map_err(db_err)?;
        rows.collect::<Result<_, _>>().map_err(db_err)
    }

    /// Payments `account` sent to others in `txid`, recovered with its
    /// outgoing viewing key: (recipient address, value).
    pub fn sent(&self, account: AccountUuid, txid: &TxId) -> Result<Vec<(String, u64)>, WalletError> {
        let conn = self.read_only()?;
        let mut stmt = conn
            .prepare(
                "SELECT to_address, value FROM v_tx_outputs
                 WHERE txid = ?1 AND from_account_uuid = ?2 AND is_change = 0
                   AND (to_account_uuid IS NULL OR to_account_uuid != ?2)",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(
                rusqlite::params![txid.as_ref().to_vec(), account.expose_uuid().as_bytes().to_vec()],
                |row| Ok((row.get::<_, Option<String>>(0)?.unwrap_or_default(), row.get::<_, i64>(1)? as u64)),
            )
            .map_err(db_err)?;
        rows.collect::<Result<_, _>>().map_err(db_err)
    }

    /// Mined transactions that spent notes belonging to `account`.
    pub fn spends_from(&self, account: AccountUuid) -> Result<Vec<TxId>, WalletError> {
        let conn = self.read_only()?;
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT txid FROM v_transactions
                 WHERE account_uuid = ?1 AND account_balance_delta < 0 AND mined_height IS NOT NULL",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([account.expose_uuid().as_bytes().to_vec()], |row| {
                let txid: Vec<u8> = row.get(0)?;
                Ok(TxId::from_bytes(txid.try_into().unwrap_or([0; 32])))
            })
            .map_err(db_err)?;
        rows.collect::<Result<_, _>>().map_err(db_err)
    }

    /// Stores a fully signed PCZT as a transaction and sends it to the network.
    pub async fn finish_and_send(
        &mut self,
        client: &mut CompactTxStreamerClient<Channel>,
        pczt: Pczt,
    ) -> Result<TxId, WalletError> {
        let txid = extract_and_store_transaction_from_pczt::<_, ()>(&mut self.db, pczt, None, None)
            .map_err(|e| WalletError::Build(format!("{e:?}")))?;
        let tx = self
            .db
            .get_transaction(txid)
            .map_err(db_err)?
            .ok_or_else(|| WalletError::Db("stored transaction vanished".into()))?;
        let mut data = Vec::new();
        tx.write(&mut data).map_err(|e| WalletError::Build(e.to_string()))?;
        let res = client
            .send_transaction(RawTransaction { data, height: 0 })
            .await
            .map_err(|e| WalletError::Grpc(e.to_string()))?
            .into_inner();
        if res.error_code != 0 {
            return Err(WalletError::Grpc(format!("rejected: {} ({})", res.error_message, res.error_code)));
        }
        Ok(txid)
    }
}

/// Adds the Ironwood (and, if present, Orchard) proofs to a PCZT.
pub fn prove(pczt: Pczt, pk: &ProvingKey) -> Result<Pczt, WalletError> {
    let mut prover = Prover::new(pczt);
    if prover.requires_ironwood_proof() {
        prover = prover
            .create_ironwood_proof(pk)
            .map_err(|e| WalletError::Prove(format!("{e:?}")))?;
    }
    if prover.requires_orchard_proof() || prover.requires_sapling_proofs() {
        return Err(WalletError::Prove(
            "transaction touches Orchard or Sapling; Tacit only builds Ironwood transactions".into(),
        ));
    }
    Ok(prover.finish())
}

/// Signs every unsigned Ironwood spend with a personal wallet's key.
pub fn sign_with_usk(pczt: Pczt, usk: &UnifiedSpendingKey) -> Result<Pczt, WalletError> {
    let request = tacit_vault::SigningRequest::from_pczt(&pczt)?;
    let ask = SpendAuthorizingKey::from(usk.orchard());
    let mut signer = Signer::new(pczt).map_err(|e| WalletError::Sign(format!("{e:?}")))?;
    for (index, _) in request.spends {
        signer
            .sign_ironwood(index, &ask)
            .map_err(|e| WalletError::Sign(format!("{e:?}")))?;
    }
    Ok(signer.finish())
}

/// Runs a complete 2 of 3 FROST signing session over a vault PCZT, with the
/// chosen signers' key packages available locally. In the product each
/// signer runs `tacit_vault::commit` and `tacit_vault::sign` on their own
/// device and only commitments and shares travel; this helper is what the
/// CLI and tests use when one machine is playing several seats.
pub fn sign_with_vault(
    pczt: Pczt,
    vault: &Vault,
    signers: &BTreeMap<Identifier, KeyPackage>,
) -> Result<Pczt, WalletError> {
    let mut round1 = BTreeMap::new();
    let mut commitments = BTreeMap::new();
    for (id, kp) in signers {
        let r1 = tacit_vault::commit(&pczt, kp, OsRng)?;
        commitments.insert(*id, r1.commitments.clone());
        round1.insert(*id, r1);
    }
    let mut shares = BTreeMap::new();
    for (id, kp) in signers {
        let r1 = round1.remove(id).expect("inserted above");
        shares.insert(*id, tacit_vault::sign(&pczt, kp, r1, &commitments)?);
    }
    Ok(tacit_vault::aggregate(pczt, vault, &commitments, &shares)?)
}
