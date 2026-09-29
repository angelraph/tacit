//! Sealed-bid auction rules.
//!
//! Everything here is a pure function of public data: the auction terms, the
//! deposits the vault received (visible to anyone with its viewing key) and
//! the drand beacon for the closing round. The seller, the signers and any
//! outside auditor all run the same code and must arrive at the same
//! settlement, which is what makes `tacit verify` possible.
//!
//! Every bid rides on a bond of exactly `terms.bond` zatoshi, so deposits are
//! indistinguishable from each other until the bids are unsealed.

use blake2b_simd::Params;

use crate::beacon::Beacon;
use crate::memo::{AuctionId, TacitMemo, RAW_ADDRESS_LEN};

/// ZIP 317 marginal fee per logical action, in zatoshi.
pub const MARGINAL_FEE: u64 = 5_000;
/// ZIP 317 grace actions: a transaction is charged for at least this many.
pub const GRACE_ACTIONS: u64 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PriceRule {
    /// The winner pays their own bid.
    FirstPrice,
    /// The winner pays the second highest bid, or the reserve if they were
    /// the only valid bidder (a Vickrey auction).
    SecondPrice,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Terms {
    /// "main", "test" or "regtest".
    pub network: String,
    /// Encoded unified full viewing key of the vault. Publishing it is what
    /// lets anyone audit the auction.
    pub vault_ufvk: String,
    /// Encoded unified address the seller is paid at.
    pub seller: String,
    /// Short description of what is being sold.
    pub item: String,
    /// Chain height the vault was created at. Auditors start scanning here.
    pub created_height: u32,
    /// Lowest acceptable price, in zatoshi.
    pub reserve: u64,
    /// The exact amount every bid must deposit, in zatoshi. Must be a ZIP 318
    /// canonical denomination and is also the highest possible bid.
    pub bond: u64,
    /// Deposits mined above this height are refunded, not counted.
    pub bids_until_height: u32,
    /// drand quicknet round the bids are sealed to.
    pub close_round: u64,
    pub rule: PriceRule,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TermsError {
    #[error("bond {0} is not a canonical denomination (1, 2 or 5 times a power of ten)")]
    BondNotCanonical(u64),
    #[error("reserve is above the bond, so no bid could ever win")]
    ReserveAboveBond,
    #[error("item description is longer than 256 bytes")]
    ItemTooLong,
}

impl Terms {
    pub fn validate(&self) -> Result<(), TermsError> {
        if !is_canonical_denomination(self.bond) {
            return Err(TermsError::BondNotCanonical(self.bond));
        }
        if self.reserve > self.bond {
            return Err(TermsError::ReserveAboveBond);
        }
        if self.item.len() > 256 {
            return Err(TermsError::ItemTooLong);
        }
        Ok(())
    }

    /// The auction id commits to every term, so changing any of them after
    /// bids are sealed produces a different auction that no bid names.
    pub fn id(&self) -> AuctionId {
        let mut h = Params::new().hash_length(32).personal(b"TacitAuction__v1").to_state();
        for field in [&self.network, &self.vault_ufvk, &self.seller, &self.item] {
            h.update(&(field.len() as u32).to_le_bytes());
            h.update(field.as_bytes());
        }
        h.update(&self.created_height.to_le_bytes());
        h.update(&self.reserve.to_le_bytes());
        h.update(&self.bond.to_le_bytes());
        h.update(&self.bids_until_height.to_le_bytes());
        h.update(&self.close_round.to_le_bytes());
        h.update(&[match self.rule {
            PriceRule::FirstPrice => 1,
            PriceRule::SecondPrice => 2,
        }]);
        h.finalize().as_bytes().try_into().unwrap()
    }
}

/// ZIP 318 canonical denominations: n * 10^k with n in {1, 2, 5}.
pub fn is_canonical_denomination(mut v: u64) -> bool {
    if v == 0 {
        return false;
    }
    while v % 10 == 0 {
        v /= 10;
    }
    matches!(v, 1 | 2 | 5)
}

/// A note the vault received, as seen through its viewing key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Deposit {
    /// Unique and public: txid followed by the action index, 36 bytes.
    pub id: Vec<u8>,
    pub value: u64,
    pub mined_height: u32,
    pub memo: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Where {
    /// A raw Orchard-protocol receiver taken from a sealed bid.
    Receiver([u8; RAW_ADDRESS_LEN]),
    /// The seller's address from the terms.
    Seller,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Payout {
    pub to: Where,
    pub amount: u64,
}

/// Why a deposit didn't count as a bid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rejection {
    /// No Tacit sealed bid for this auction in the memo; there is nowhere to
    /// refund it, so it goes to the seller.
    Unreadable,
    Late,
    WrongBond,
    BidAboveBond,
    BelowReserve,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenedDeposit {
    pub deposit_id: Vec<u8>,
    pub bid: Option<u64>,
    pub rejected: Option<Rejection>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub auction: AuctionId,
    /// Deposit id of the winning bid, if any bid was valid.
    pub winner: Option<Vec<u8>>,
    /// What the winner pays, before the fee comes out of the seller's share.
    pub price: Option<u64>,
    pub fee: u64,
    pub payouts: Vec<Payout>,
    pub opened: Vec<OpenedDeposit>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SettleError {
    #[error("beacon is for round {got}, auction closes at round {want}")]
    WrongBeacon { want: u64, got: u64 },
    #[error("deposits don't cover the settlement fee")]
    CannotCoverFee,
    #[error("invalid terms: {0}")]
    Terms(#[from] TermsError),
}

/// Computes the settlement. Deterministic: same inputs, same outcome.
pub fn settle(terms: &Terms, deposits: &[Deposit], beacon: &Beacon) -> Result<Outcome, SettleError> {
    terms.validate()?;
    if beacon.round() != terms.close_round {
        return Err(SettleError::WrongBeacon { want: terms.close_round, got: beacon.round() });
    }
    let auction = terms.id();

    struct Valid<'a> {
        dep: &'a Deposit,
        bid: u64,
        refund_to: [u8; RAW_ADDRESS_LEN],
        tiebreak: [u8; 32],
    }
    let mut valid = Vec::new();
    let mut refunds: Vec<Payout> = Vec::new();
    let mut to_seller_unreadable = 0u64;
    let mut opened = Vec::new();

    for dep in deposits {
        let bid = match TacitMemo::parse(&dep.memo) {
            Ok(TacitMemo::SealedBid(sb)) if sb.auction == auction => sb.open(beacon).ok(),
            _ => None,
        };
        let Some(bid) = bid else {
            to_seller_unreadable += dep.value;
            opened.push(OpenedDeposit { deposit_id: dep.id.clone(), bid: None, rejected: Some(Rejection::Unreadable) });
            continue;
        };
        let rejected = if dep.mined_height > terms.bids_until_height {
            Some(Rejection::Late)
        } else if dep.value != terms.bond {
            Some(Rejection::WrongBond)
        } else if bid.amount > terms.bond {
            Some(Rejection::BidAboveBond)
        } else if bid.amount < terms.reserve {
            Some(Rejection::BelowReserve)
        } else {
            None
        };
        opened.push(OpenedDeposit { deposit_id: dep.id.clone(), bid: Some(bid.amount), rejected: rejected.clone() });
        if rejected.is_some() {
            refunds.push(Payout { to: Where::Receiver(bid.refund_to), amount: dep.value });
        } else {
            let tiebreak = Params::new()
                .hash_length(32)
                .personal(b"TacitTiebreak_v1")
                .to_state()
                .update(&beacon.randomness())
                .update(&dep.id)
                .finalize()
                .as_bytes()
                .try_into()
                .unwrap();
            valid.push(Valid { dep, bid: bid.amount, refund_to: bid.refund_to, tiebreak });
        }
    }

    // Highest bid first; ties broken by beacon randomness nobody could predict.
    valid.sort_by(|a, b| b.bid.cmp(&a.bid).then(a.tiebreak.cmp(&b.tiebreak)));

    let (winner, price) = match valid.first() {
        None => (None, None),
        Some(top) => {
            let price = match terms.rule {
                PriceRule::FirstPrice => top.bid,
                PriceRule::SecondPrice => valid.get(1).map_or(terms.reserve, |second| second.bid).max(terms.reserve),
            };
            (Some(top.dep.id.clone()), Some(price))
        }
    };

    let mut payouts = refunds;
    for (i, v) in valid.iter().enumerate() {
        let back = if i == 0 { v.dep.value - price.unwrap() } else { v.dep.value };
        if back > 0 {
            payouts.push(Payout { to: Where::Receiver(v.refund_to), amount: back });
        }
    }
    let seller_gross = price.unwrap_or(0) + to_seller_unreadable;

    // The fee depends on how many outputs there are, and whether the seller
    // gets an output depends on the fee, so settle it in two passes.
    //
    // The settlement also always carries one zero-valued change output back
    // to the vault. librustzcash adds it so that spend-everything
    // transactions look like any other, and it counts toward the fee.
    let outputs_without_seller = payouts.len() as u64;
    let fee_for =
        |outputs: u64| MARGINAL_FEE * GRACE_ACTIONS.max((outputs + 1).max(deposits.len() as u64));
    let mut fee = fee_for(outputs_without_seller + 1);
    if seller_gross > fee {
        payouts.push(Payout { to: Where::Seller, amount: seller_gross - fee });
    } else {
        // Nothing (or too little) for the seller: the fee comes out of the
        // refunds, evenly, with any remainder taken from the first one.
        fee = fee_for(outputs_without_seller);
        let mut owed = fee - seller_gross;
        let n = payouts.len() as u64;
        if n == 0 {
            return Err(SettleError::CannotCoverFee);
        }
        let share = owed / n;
        for p in payouts.iter_mut() {
            if p.amount < share {
                return Err(SettleError::CannotCoverFee);
            }
            p.amount -= share;
            owed -= share;
        }
        let first = payouts.first_mut().unwrap();
        if first.amount < owed {
            return Err(SettleError::CannotCoverFee);
        }
        first.amount -= owed;
    }

    Ok(Outcome { auction, winner, price, fee, payouts, opened })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memo::{Bid, SealedBid};

    const ROUND: u64 = 32_633_680;
    const SIG: &str = "980de8271fc02d5191a0e8977922c347f5dbad595e2d080117ab64385a341b79cfb7ebef0459b5260209dc77259d8121";
    const ZEC: u64 = 100_000_000;

    fn beacon() -> Beacon {
        Beacon::verify(ROUND, &hex::decode(SIG).unwrap()).unwrap()
    }

    fn terms(rule: PriceRule) -> Terms {
        Terms {
            network: "regtest".into(),
            vault_ufvk: "uviewregtest1example".into(),
            seller: "uregtest1seller".into(),
            item: "a rare thing".into(),
            created_height: 10,
            reserve: ZEC / 10,
            bond: ZEC,
            bids_until_height: 100,
            close_round: ROUND,
            rule,
        }
    }

    fn deposit(t: &Terms, n: u8, amount: u64) -> Deposit {
        let bid = Bid { auction: t.id(), amount, refund_to: [n; RAW_ADDRESS_LEN] };
        Deposit {
            id: vec![n; 36],
            value: t.bond,
            mined_height: 50,
            memo: SealedBid::seal(&bid, t.close_round).unwrap().to_memo().to_vec(),
        }
    }

    fn total_out(o: &Outcome) -> u64 {
        o.payouts.iter().map(|p| p.amount).sum::<u64>() + o.fee
    }

    #[test]
    fn canonical_denominations() {
        for v in [1, 2, 5, 10, 20, 50, 100_000_000, 200_000_000, 500] {
            assert!(is_canonical_denomination(v), "{v}");
        }
        for v in [0, 3, 15, 25, 120_000_000, 99] {
            assert!(!is_canonical_denomination(v), "{v}");
        }
    }

    #[test]
    fn changing_any_term_changes_the_id() {
        let t = terms(PriceRule::FirstPrice);
        let mut u = t.clone();
        u.reserve += 1;
        assert_ne!(t.id(), u.id());
        let mut u = t.clone();
        u.rule = PriceRule::SecondPrice;
        assert_ne!(t.id(), u.id());
    }

    #[test]
    fn first_price_pays_the_top_bid_and_refunds_everyone_else() {
        let t = terms(PriceRule::FirstPrice);
        let deps = [deposit(&t, 1, 3 * ZEC / 10), deposit(&t, 2, 7 * ZEC / 10), deposit(&t, 3, 5 * ZEC / 10)];
        let o = settle(&t, &deps, &beacon()).unwrap();
        assert_eq!(o.winner, Some(vec![2; 36]));
        assert_eq!(o.price, Some(7 * ZEC / 10));
        assert_eq!(total_out(&o), 3 * ZEC, "every zatoshi deposited is paid out or is the fee");
        assert!(o.payouts.contains(&Payout { to: Where::Receiver([2; 43]), amount: 3 * ZEC / 10 }));
        assert!(o.payouts.contains(&Payout { to: Where::Receiver([1; 43]), amount: ZEC }));
        assert!(o.payouts.contains(&Payout { to: Where::Seller, amount: 7 * ZEC / 10 - o.fee }));
    }

    #[test]
    fn second_price_pays_the_runner_up_bid() {
        let t = terms(PriceRule::SecondPrice);
        let deps = [deposit(&t, 1, 3 * ZEC / 10), deposit(&t, 2, 7 * ZEC / 10), deposit(&t, 3, 5 * ZEC / 10)];
        let o = settle(&t, &deps, &beacon()).unwrap();
        assert_eq!(o.winner, Some(vec![2; 36]));
        assert_eq!(o.price, Some(5 * ZEC / 10));
    }

    #[test]
    fn a_lone_vickrey_bidder_pays_the_reserve() {
        let t = terms(PriceRule::SecondPrice);
        let o = settle(&t, &[deposit(&t, 1, 9 * ZEC / 10)], &beacon()).unwrap();
        assert_eq!(o.price, Some(t.reserve));
    }

    #[test]
    fn late_low_and_foreign_deposits_are_handled() {
        let t = terms(PriceRule::FirstPrice);
        let mut late = deposit(&t, 1, 9 * ZEC / 10);
        late.mined_height = 101;
        let low = deposit(&t, 2, ZEC / 100);
        let mut other_auction = t.clone();
        other_auction.item = "something else".into();
        let foreign = deposit(&other_auction, 3, ZEC / 2);
        let ok = deposit(&t, 4, ZEC / 2);
        let o = settle(&t, &[late, low, foreign, ok], &beacon()).unwrap();
        assert_eq!(o.winner, Some(vec![4; 36]));
        let why = |n: u8| o.opened.iter().find(|d| d.deposit_id == vec![n; 36]).unwrap().rejected.clone();
        assert_eq!(why(1), Some(Rejection::Late));
        assert_eq!(why(2), Some(Rejection::BelowReserve));
        assert_eq!(why(3), Some(Rejection::Unreadable), "a bid sealed for another auction can't be read here");
        assert_eq!(total_out(&o), 4 * ZEC);
    }

    #[test]
    fn no_valid_bids_refunds_everyone_minus_the_fee() {
        let t = terms(PriceRule::FirstPrice);
        let deps = [deposit(&t, 1, ZEC / 100), deposit(&t, 2, ZEC / 100)];
        let o = settle(&t, &deps, &beacon()).unwrap();
        assert_eq!(o.winner, None);
        assert!(o.payouts.iter().all(|p| p.to != Where::Seller));
        assert_eq!(total_out(&o), 2 * ZEC);
    }

    #[test]
    fn ties_are_broken_the_same_way_every_time() {
        let t = terms(PriceRule::FirstPrice);
        let deps = [deposit(&t, 1, ZEC / 2), deposit(&t, 2, ZEC / 2), deposit(&t, 3, ZEC / 2)];
        let a = settle(&t, &deps, &beacon()).unwrap();
        let reversed: Vec<_> = deps.iter().rev().cloned().collect();
        let b = settle(&t, &reversed, &beacon()).unwrap();
        assert_eq!(a.winner, b.winner, "deposit order must not matter");
    }

    #[test]
    fn refuses_the_wrong_beacon() {
        let mut t = terms(PriceRule::FirstPrice);
        t.close_round += 1;
        assert!(matches!(settle(&t, &[], &beacon()), Err(SettleError::WrongBeacon { .. })));
    }
}
