//! Auditing an auction from its published terms alone.
//!
//! This is the same check as `tacit auction verify`, done with nothing but
//! the vault's viewing key from the terms, public chain data and the drand
//! beacon, so it can run in any browser. It finds every deposit, opens the
//! sealed bids once the beacon exists, recomputes the settlement the rules
//! demand, recovers what the vault actually paid with its outgoing viewing
//! key, and compares the two.

use std::collections::{BTreeMap, HashMap};

use orchard::keys::{OutgoingViewingKey, PreparedIncomingViewingKey, Scope};
use orchard::note_encryption::IronwoodDomain;
use serde::Serialize;
use tacit_core::auction::{self, Deposit, Rejection, Terms, Where};
use tacit_core::beacon::Beacon;
use tacit_core::memo::TacitMemo;
use zcash_note_encryption::{try_note_decryption, try_output_recovery_with_ovk};
use zcash_primitives::transaction::Transaction;
use zcash_protocol::consensus::{BlockHeight, BranchId};

use crate::chain::{self, RawTx};
use crate::keys::{decode_fvk, decode_raw_address, encode_raw_address, vault_address};
use crate::{invalid, Error, Net, Result, Scanner};

pub struct Audit {
    terms: Terms,
    net: Net,
    scanner: Scanner,
    ivk: PreparedIncomingViewingKey,
    ovks: [OutgoingViewingKey; 2],
    /// Memos of deposits, by deposit id.
    memos: HashMap<Vec<u8>, Vec<u8>>,
    /// What the settlement paid, once its transaction has been read.
    paid: Option<Vec<([u8; 43], u64)>>,
}

impl Audit {
    /// Starts an audit. `birthday` is the tree state at `terms.created_height`.
    pub fn new(terms: Terms, birthday: &chain::Tree) -> Result<Self> {
        terms.validate().map_err(invalid)?;
        let net = Net::parse(&terms.network).ok_or_else(|| invalid("unknown network in terms"))?;
        let fvk = decode_fvk(net, &terms.vault_ufvk)?;
        let ivk = fvk.to_ivk(Scope::External).prepare();
        let ovks = [fvk.to_ovk(Scope::External), fvk.to_ovk(Scope::Internal)];
        let scanner = Scanner::watch_only(net, fvk, birthday)?;
        Ok(Audit { terms, net, scanner, ivk, ovks, memos: HashMap::new(), paid: None })
    }

    pub fn terms(&self) -> &Terms {
        &self.terms
    }

    pub fn scanner(&mut self) -> &mut Scanner {
        &mut self.scanner
    }

    pub fn height(&self) -> u32 {
        self.scanner.height()
    }

    pub fn vault_address(&self) -> String {
        encode_raw_address(self.net, &vault_address(self.scanner.fvk()).to_raw_address_bytes())
    }

    fn deposits(&self) -> impl Iterator<Item = &crate::scan::Found> {
        // Notes the vault sent itself (the settlement's change) aren't deposits.
        let spends: Vec<&String> = self.scanner.spends().iter().map(|s| &s.0).collect();
        self.scanner.found().iter().filter(move |f| !f.change && !spends.contains(&&f.txid))
    }

    /// Transactions whose full data the audit still needs: deposits (for
    /// their memos) and the settlement (for its payments).
    pub fn wanted(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .deposits()
            .filter(|f| !self.memos.contains_key(&f.deposit_id()))
            .map(|f| f.txid.clone())
            .collect();
        if self.paid.is_none() {
            if let Some((txid, _)) = self.scanner.spends().first() {
                out.push(txid.clone());
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// Reads one full transaction fetched for [`Self::wanted`].
    pub fn add_tx(&mut self, txid: &str, raw: &RawTx) -> Result<()> {
        let height = if raw.height == 0 { u32::MAX } else { raw.height as u32 };
        let branch = BranchId::for_height(&self.net, BlockHeight::from_u32(height));
        let data = hex::decode(&raw.data).map_err(|_| Error::Chain("transaction is not hex".into()))?;
        let tx = Transaction::read(&data[..], branch).map_err(|e| Error::Chain(format!("bad transaction: {e}")))?;
        if tx.txid().to_string() != txid {
            return Err(Error::Chain("the server sent a different transaction".into()));
        }
        let Some(bundle) = tx.ironwood_bundle() else {
            return Err(Error::Chain("transaction has no Ironwood part".into()));
        };
        let wanted: Vec<(Vec<u8>, u32)> = self
            .deposits()
            .filter(|f| f.txid == txid)
            .map(|f| (f.deposit_id(), f.action))
            .collect();
        for (id, index) in wanted {
            let action = bundle
                .actions()
                .get(index as usize)
                .ok_or_else(|| Error::Chain("deposit action missing".into()))?;
            let domain = IronwoodDomain::for_action(action);
            let (_, _, memo) = try_note_decryption(&domain, &self.ivk, action)
                .ok_or_else(|| Error::Chain("can't decrypt a deposit the compact scan found".into()))?;
            self.memos.insert(id, memo.to_vec());
        }
        if self.scanner.spends().first().map(|s| s.0.as_str()) == Some(txid) {
            let own = vault_address(self.scanner.fvk()).to_raw_address_bytes();
            let mut paid = Vec::new();
            for action in bundle.actions() {
                let domain = IronwoodDomain::for_action(action);
                let recovered = self.ovks.iter().find_map(|ovk| {
                    try_output_recovery_with_ovk(
                        &domain,
                        ovk,
                        action,
                        action.cv_net(),
                        &action.encrypted_note().out_ciphertext,
                    )
                });
                if let Some((note, addr, _)) = recovered {
                    let value = note.value().inner();
                    let raw = addr.to_raw_address_bytes();
                    // Zero-valued padding and the vault's empty change note
                    // carry nothing.
                    if value > 0 && raw != own && !self.is_vault_internal(&addr) {
                        paid.push((raw, value));
                    }
                }
            }
            self.paid = Some(paid);
        }
        Ok(())
    }

    fn is_vault_internal(&self, addr: &orchard::Address) -> bool {
        self.scanner.fvk().scope_for_address(addr) == Some(Scope::Internal)
    }

    /// Everything known so far. Pass the beacon for the closing round once
    /// it exists to open the bids and check the settlement.
    pub fn report(&self, beacon: Option<&Beacon>) -> Result<Report> {
        let deposits: Vec<Deposit> = self
            .deposits()
            .map(|f| Deposit {
                id: f.deposit_id(),
                value: f.value(),
                mined_height: f.height,
                memo: self.memos.get(&f.deposit_id()).cloned().unwrap_or_default(),
            })
            .collect();
        let auction_id = self.terms.id();
        let mut rows: Vec<DepositRow> = deposits
            .iter()
            .map(|d| DepositRow {
                id: hex::encode(&d.id),
                value: d.value,
                height: d.mined_height,
                sealed_bid: matches!(TacitMemo::parse(&d.memo), Ok(TacitMemo::SealedBid(sb)) if sb.auction == auction_id),
                bid: None,
                status: if d.mined_height > self.terms.bids_until_height { "late".into() } else { "sealed".into() },
            })
            .collect();

        let mut report = Report {
            auction: hex::encode(auction_id),
            vault: self.vault_address(),
            scanned_to: self.height(),
            deposits: vec![],
            memos_complete: deposits.iter().all(|d| self.memos.contains_key(&d.id)),
            outcome: None,
            settlement: self.scanner.spends().first().map(|(txid, height)| SettlementRow {
                txid: txid.clone(),
                height: *height,
                payments: self
                    .paid
                    .as_ref()
                    .map(|p| p.iter().map(|(raw, v)| Payment { to: encode_raw_address(self.net, raw), amount: *v }).collect()),
            }),
            missing: vec![],
            unexpected: vec![],
            verdict: Verdict::Open,
        };

        let Some(beacon) = beacon else {
            report.deposits = rows;
            return Ok(report);
        };
        if !report.memos_complete {
            report.deposits = rows;
            return Err(Error::Chain("fetch every deposit transaction before settling".into()));
        }
        // Nobody bid: there is nothing to settle, and a vault that paid
        // anything out anyway would be a mismatch.
        if deposits.is_empty() {
            report.verdict = if self.scanner.spends().is_empty() { Verdict::NoBids } else { Verdict::Mismatch };
            return Ok(report);
        }
        let outcome = auction::settle(&self.terms, &deposits, beacon).map_err(invalid)?;
        for (row, opened) in rows.iter_mut().zip(&outcome.opened) {
            row.bid = opened.bid;
            row.status = match (&opened.rejected, outcome.winner.as_ref() == Some(&opened.deposit_id)) {
                (_, true) => "won".into(),
                (None, false) => "outbid".into(),
                (Some(Rejection::Unreadable), _) => "unreadable".into(),
                (Some(Rejection::Late), _) => "late".into(),
                (Some(Rejection::WrongBond), _) => "wrong bond".into(),
                (Some(Rejection::BidAboveBond), _) => "above bond".into(),
                (Some(Rejection::BelowReserve), _) => "below reserve".into(),
            };
        }
        report.deposits = rows;
        let seller = decode_raw_address(self.net, &self.terms.seller)?;
        let expected: Vec<([u8; 43], u64)> = outcome
            .payouts
            .iter()
            .map(|p| match &p.to {
                Where::Seller => (seller, p.amount),
                Where::Receiver(raw) => (*raw, p.amount),
            })
            .collect();
        report.outcome = Some(OutcomeRow {
            winner: outcome.winner.as_ref().map(hex::encode),
            price: outcome.price,
            fee: outcome.fee,
            payouts: expected
                .iter()
                .map(|(raw, v)| Payment { to: encode_raw_address(self.net, raw), amount: *v })
                .collect(),
        });
        match &self.paid {
            None => report.verdict = Verdict::AwaitingSettlement,
            Some(paid) => {
                let (missing, unexpected) = diff(&expected, paid);
                report.verdict = if missing.is_empty() && unexpected.is_empty() { Verdict::Honest } else { Verdict::Mismatch };
                report.missing = missing.iter().map(|(r, v)| Payment { to: encode_raw_address(self.net, r), amount: *v }).collect();
                report.unexpected =
                    unexpected.iter().map(|(r, v)| Payment { to: encode_raw_address(self.net, r), amount: *v }).collect();
            }
        }
        if self.scanner.spends().len() > 1 {
            report.verdict = Verdict::Mismatch;
        }
        Ok(report)
    }
}

fn diff<T: Ord + Clone>(expected: &[T], actual: &[T]) -> (Vec<T>, Vec<T>) {
    let mut left: BTreeMap<&T, i64> = BTreeMap::new();
    for e in expected {
        *left.entry(e).or_default() += 1;
    }
    for a in actual {
        *left.entry(a).or_default() -= 1;
    }
    let (mut missing, mut unexpected) = (vec![], vec![]);
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

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Bids are still sealed (no beacon yet).
    Open,
    /// The bids are open, but the vault hasn't paid out yet.
    AwaitingSettlement,
    /// The vault paid exactly what the rules require.
    Honest,
    /// The bids unsealed and there were none; the vault never moved.
    NoBids,
    /// The vault paid something else.
    Mismatch,
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub auction: String,
    pub vault: String,
    pub scanned_to: u32,
    pub deposits: Vec<DepositRow>,
    pub memos_complete: bool,
    pub outcome: Option<OutcomeRow>,
    pub settlement: Option<SettlementRow>,
    pub missing: Vec<Payment>,
    pub unexpected: Vec<Payment>,
    pub verdict: Verdict,
}

#[derive(Clone, Debug, Serialize)]
pub struct DepositRow {
    pub id: String,
    pub value: u64,
    pub height: u32,
    pub sealed_bid: bool,
    pub bid: Option<u64>,
    pub status: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct OutcomeRow {
    pub winner: Option<String>,
    pub price: Option<u64>,
    pub fee: u64,
    pub payouts: Vec<Payment>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SettlementRow {
    pub txid: String,
    pub height: u32,
    pub payments: Option<Vec<Payment>>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Payment {
    pub to: String,
    pub amount: u64,
}
