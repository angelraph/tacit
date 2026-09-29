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
    propose_standard_transfer_to_address, ConfirmationsPolicy,
};
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
use zcash_keys::keys::{UnifiedAddressRequest, UnifiedSpendingKey};
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

pub mod cache;
use cache::MemBlockCache;

pub type Db<P> = WalletDb<rusqlite::Connection, P, SystemClock, OsRng>;

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
    params: P,
    cache: MemBlockCache,
}

impl<P: Parameters + Clone + Send + Sync + 'static> Wallet<P> {
    pub fn open(path: impl AsRef<Path>, params: P) -> Result<Self, WalletError> {
        let mut db = WalletDb::for_path(path, params.clone(), SystemClock, OsRng).map_err(db_err)?;
        init_wallet_db(&mut db, None).map_err(db_err)?;
        Ok(Self { db, params, cache: MemBlockCache::new() })
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
        let account = self
            .db
            .import_account_ufvk(
                name,
                &vault.ufvk(),
                birthday,
                AccountPurpose::Spending { derivation: None },
                None,
            )
            .map_err(db_err)?;
        Ok(account.id())
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
            .map_err(|e| WalletError::Sync(format!("{e:?}")))
    }

    /// Builds an unsigned, unproven PCZT paying `amount` from `account` to `to`.
    pub fn propose(
        &mut self,
        account: AccountUuid,
        to: &str,
        amount: u64,
        memo: Option<[u8; 512]>,
    ) -> Result<Pczt, WalletError> {
        let to = Address::decode(&self.params, to)
            .ok_or_else(|| WalletError::Build(format!("not an address on this network: {to}")))?;
        let amount = Zatoshis::from_u64(amount).map_err(|_| WalletError::Build("bad amount".into()))?;
        let memo = memo.map(|m| MemoBytes::from_bytes(&m).expect("512 bytes is a valid memo"));
        let proposal = propose_standard_transfer_to_address::<_, _, Infallible>(
            &mut self.db,
            &self.params,
            StandardFeeRule::Zip317,
            account,
            ConfirmationsPolicy::MIN,
            &to,
            amount,
            memo,
            None,
            ShieldedPool::Ironwood,
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
