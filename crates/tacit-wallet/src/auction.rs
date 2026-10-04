//! Auction operations on top of the wallet: bidding, building the settlement,
//! and verifying an auction from nothing but its published terms.

use std::collections::BTreeMap;
use std::path::Path;

use orchard::circuit::ProvingKey;
use tacit_core::auction::{self, Deposit, Outcome, Terms, Where};
use tacit_core::beacon::Beacon;
use tacit_core::memo::{Bid, SealedBid};
use tonic::transport::Channel;
use zcash_client_backend::data_api::AccountPurpose;
use zcash_client_backend::proto::service::compact_tx_streamer_client::CompactTxStreamerClient;
use zcash_client_backend::proto::service::BlockId;
use zcash_client_backend::data_api::AccountBirthday;
use zcash_client_sqlite::AccountUuid;
use zcash_keys::keys::{UnifiedFullViewingKey, UnifiedSpendingKey};
use zcash_primitives::transaction::TxId;
use zcash_protocol::consensus::Parameters;

use crate::{address_from_raw, prove, raw_receiver, sign_with_usk, Wallet, WalletError};

#[derive(Debug, thiserror::Error)]
pub enum AuctionError {
    #[error(transparent)]
    Wallet(#[from] WalletError),
    #[error("terms: {0}")]
    Terms(#[from] auction::TermsError),
    #[error("settlement: {0}")]
    Settle(#[from] auction::SettleError),
    #[error("{0}")]
    Other(String),
}

/// Seals a bid and deposits the bond into the auction's vault. The refund
/// address is a fresh diversified address of the bidder's own account.
pub async fn place_bid<P: Parameters + Clone + Send + Sync + 'static>(
    wallet: &mut Wallet<P>,
    client: &mut CompactTxStreamerClient<Channel>,
    pk: &ProvingKey,
    bidder: AccountUuid,
    usk: &UnifiedSpendingKey,
    terms: &Terms,
    vault_address: &str,
    amount: u64,
) -> Result<TxId, AuctionError> {
    terms.validate()?;
    let refund = wallet.next_address(bidder)?;
    let refund_to = raw_receiver(wallet.params(), &refund)
        .ok_or_else(|| AuctionError::Other("refund address has no shielded receiver".into()))?;
    let bid = Bid { auction: terms.id(), amount, refund_to };
    let memo = SealedBid::seal(&bid, terms.close_round)
        .map_err(|e| AuctionError::Other(e.to_string()))?
        .to_memo();
    let pczt = wallet.propose(bidder, vault_address, terms.bond, Some(memo))?;
    let pczt = sign_with_usk(prove(pczt, pk)?, usk)?;
    Ok(wallet.finish_and_send(client, pczt).await?)
}

/// The vault's deposits in the form the auction rules take.
pub fn deposits<P: Parameters + Clone + Send + Sync + 'static>(
    wallet: &Wallet<P>,
    vault: AccountUuid,
) -> Result<Vec<Deposit>, AuctionError> {
    Ok(wallet
        .received(vault)?
        .into_iter()
        .map(|r| Deposit { id: r.id, value: r.value, mined_height: r.mined_height, memo: r.memo })
        .collect())
}

/// Turns an outcome into (address, amount) payments.
pub fn settlement_payments(params: &impl Parameters, terms: &Terms, outcome: &Outcome) -> Vec<(String, u64)> {
    outcome
        .payouts
        .iter()
        .map(|p| {
            let to = match &p.to {
                Where::Seller => terms.seller.clone(),
                Where::Receiver(raw) => address_from_raw(params, raw),
            };
            (to, p.amount)
        })
        .collect()
}

/// Result of checking an auction from the chain.
#[derive(Debug)]
pub struct Verdict {
    pub outcome: Outcome,
    /// The settlement transaction, if the vault has spent anything yet.
    pub settlement: Option<TxId>,
    /// Payments the rules require that the settlement doesn't make.
    pub missing: Vec<(String, u64)>,
    /// Payments the settlement makes that the rules don't call for.
    pub unexpected: Vec<(String, u64)>,
}

impl Verdict {
    pub fn is_honest(&self) -> bool {
        self.settlement.is_some() && self.missing.is_empty() && self.unexpected.is_empty()
    }
}

/// Audits an auction using only its public terms, the chain and drand.
///
/// Imports the vault's viewing key into a fresh, separate wallet, scans from
/// the vault's creation height, recomputes the settlement the rules demand,
/// and compares it with what the vault actually paid out.
pub async fn verify<P: Parameters + Clone + Send + Sync + 'static>(
    params: P,
    client: &mut CompactTxStreamerClient<Channel>,
    terms: &Terms,
    beacon: &Beacon,
    scratch_db: &Path,
) -> Result<Verdict, AuctionError> {
    terms.validate()?;
    let ufvk = UnifiedFullViewingKey::decode(&params, &terms.vault_ufvk)
        .map_err(|e| AuctionError::Other(format!("terms carry a bad viewing key: {e}")))?;
    let mut wallet = Wallet::open(scratch_db, params.clone())?;
    let ts = client
        .get_tree_state(BlockId { height: terms.created_height as u64, hash: vec![] })
        .await
        .map_err(|e| WalletError::Grpc(e.to_string()))?
        .into_inner();
    let birthday = AccountBirthday::from_treestate(ts, None)
        .map_err(|_| WalletError::Grpc("bad tree state".into()))?;
    let vault = wallet.import_ufvk("audited vault", &ufvk, &birthday, AccountPurpose::ViewOnly)?;
    wallet.sync(client).await?;

    let outcome = auction::settle(terms, &deposits(&wallet, vault)?, beacon)?;
    let expected = settlement_payments(&params, terms, &outcome);

    let spends = wallet.spends_from(vault)?;
    let Some(settlement) = spends.first().copied() else {
        return Ok(Verdict { outcome, settlement: None, missing: expected, unexpected: vec![] });
    };
    if spends.len() > 1 {
        return Err(AuctionError::Other(format!(
            "vault spent in {} transactions; a Tacit vault settles in exactly one",
            spends.len()
        )));
    }
    let actual = wallet.sent(vault, &settlement)?;

    let (missing, unexpected) = diff(&expected, &actual);
    Ok(Verdict { outcome, settlement: Some(settlement), missing, unexpected })
}

/// What a cosigner checks before signing a settlement: the PCZT's
/// value-carrying Ironwood outputs must be exactly the payments the rules
/// demand (`expected`, from [`settlement_payments`]), and it must spend
/// `deposits` notes, no more and no fewer. Zero-value outputs (padding and the
/// empty change note) are allowed. Returns the problems found, if any.
pub fn check_settlement(
    params: &impl Parameters,
    pczt: &pczt::Pczt,
    expected: &[(String, u64)],
    deposits: usize,
) -> Result<(), String> {
    let mut actual = Vec::new();
    let mut spends = 0;
    pczt::roles::verifier::Verifier::new(pczt.clone())
        .with_ironwood(|bundle| -> Result<(), pczt::roles::verifier::OrchardError<()>> {
            for action in bundle.actions() {
                if action.spend().value().map_or(0, |v| v.inner()) > 0 {
                    spends += 1;
                }
                let out = action.output();
                let value = out.value().map(|v| v.inner());
                match (value, out.recipient()) {
                    (Some(0), _) => {}
                    (Some(v), Some(to)) => actual.push((address_from_raw(params, &to.to_raw_address_bytes()), v)),
                    _ => actual.push(("<hidden recipient or value>".into(), value.unwrap_or(0))),
                }
            }
            Ok(())
        })
        .map_err(|e| format!("can't read the Ironwood bundle: {e:?}"))?;

    // Compare by raw receiver so different encodings of one address match.
    let canon = |list: &[(String, u64)]| -> Vec<(String, u64)> {
        list.iter()
            .map(|(a, v)| (raw_receiver(params, a).map(|r| address_from_raw(params, &r)).unwrap_or(a.clone()), *v))
            .collect()
    };
    let (missing, unexpected) = diff(&canon(expected), &canon(&actual));
    let mut problems = Vec::new();
    for (a, v) in &missing {
        problems.push(format!("missing payment of {v} zat to {a}"));
    }
    for (a, v) in &unexpected {
        problems.push(format!("unexpected payment of {v} zat to {a}"));
    }
    if spends != deposits {
        problems.push(format!("spends {spends} notes but the vault holds {deposits} deposits"));
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    }
}

/// Multiset difference both ways.
fn diff(expected: &[(String, u64)], actual: &[(String, u64)]) -> (Vec<(String, u64)>, Vec<(String, u64)>) {
    let mut left: BTreeMap<&(String, u64), i64> = BTreeMap::new();
    for e in expected {
        *left.entry(e).or_default() += 1;
    }
    for a in actual {
        *left.entry(a).or_default() -= 1;
    }
    let mut missing = vec![];
    let mut unexpected = vec![];
    for (k, n) in left {
        for _ in 0..n.max(0) {
            missing.push(k.clone());
        }
        for _ in 0..(-n).max(0) {
            unexpected.push(k.clone());
        }
    }
    (missing, unexpected)
}
