//! Shielded escrow rules.
//!
//! A buyer pays into a 2 of 3 FROST vault whose seats are the buyer, the
//! seller and an arbiter. The vault only ever makes one payment: everything
//! it holds goes either to the seller (the deal went through) or back to the
//! buyer (it didn't). Any two seats can make that payment, and the rules for
//! an unattended arbiter are simple enough to automate:
//!
//! - if the **buyer** asks to pay the seller, cosign: the buyer is giving up
//!   their own claim;
//! - if the **seller** asks to refund the buyer, cosign: the seller is giving
//!   up theirs;
//! - anything else is a dispute and needs the arbiter's own decision.
//!
//! Like auctions, the terms carry the vault's viewing key, so anyone can
//! check where the money went without learning who the parties are.

use blake2b_simd::Params;

use crate::auction::{GRACE_ACTIONS, MARGINAL_FEE};

pub type EscrowId = [u8; 32];

/// Seat order in an escrow vault.
pub const BUYER: u16 = 1;
pub const SELLER: u16 = 2;
pub const ARBITER: u16 = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct EscrowTerms {
    /// "main", "test" or "regtest".
    pub network: String,
    /// The vault's unified full viewing key (public, for auditing).
    pub vault_ufvk: String,
    /// Hex X25519 seat keys of buyer, seller and arbiter, in that order.
    pub seats: Vec<String>,
    /// Unified address the buyer is refunded at.
    pub buyer: String,
    /// Unified address the seller is paid at.
    pub seller: String,
    /// What the escrow is for, at most 256 bytes.
    pub item: String,
    /// What the buyer pays in, in zatoshi.
    pub amount: u64,
    /// Chain height the vault was created at.
    pub created_height: u32,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EscrowError {
    #[error("description is longer than 256 bytes")]
    ItemTooLong,
    #[error("an escrow needs exactly three distinct seat keys")]
    Seats,
    #[error("amount must be positive and cover the fee")]
    Amount,
}

impl EscrowTerms {
    pub fn validate(&self) -> Result<(), EscrowError> {
        if self.item.len() > 256 {
            return Err(EscrowError::ItemTooLong);
        }
        let ok = |s: &String| hex::decode(s).map(|b| b.len() == 32).unwrap_or(false);
        if self.seats.len() != 3
            || !self.seats.iter().all(ok)
            || self.seats[0] == self.seats[1]
            || self.seats[0] == self.seats[2]
            || self.seats[1] == self.seats[2]
        {
            return Err(EscrowError::Seats);
        }
        if self.amount <= fee(1) {
            return Err(EscrowError::Amount);
        }
        Ok(())
    }

    pub fn id(&self) -> EscrowId {
        let mut h = Params::new().hash_length(32).personal(b"TacitEscrow___v1").to_state();
        let mut put = |s: &str| {
            h.update(&(s.len() as u32).to_le_bytes());
            h.update(s.as_bytes());
        };
        put(&self.network);
        put(&self.vault_ufvk);
        for s in &self.seats {
            put(s);
        }
        put(&self.buyer);
        put(&self.seller);
        put(&self.item);
        h.update(&self.amount.to_le_bytes());
        h.update(&self.created_height.to_le_bytes());
        h.finalize().as_bytes().try_into().unwrap()
    }

    /// Seat number (1 to 3) of a seat key, if it's one of this escrow's.
    pub fn seat_of(&self, key_hex: &str) -> Option<u16> {
        self.seats.iter().position(|s| s.eq_ignore_ascii_case(key_hex)).map(|i| i as u16 + 1)
    }
}

/// Who the vault pays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(rename_all = "snake_case"))]
pub enum Release {
    /// The deal went through: everything to the seller.
    ToSeller,
    /// It didn't: everything back to the buyer.
    ToBuyer,
}

impl Release {
    pub fn parse(s: &str) -> Option<Release> {
        match s {
            "seller" | "to_seller" => Some(Release::ToSeller),
            "buyer" | "to_buyer" => Some(Release::ToBuyer),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Release::ToSeller => "seller",
            Release::ToBuyer => "buyer",
        }
    }
}

/// ZIP 317 fee for spending `notes` vault notes into one payment plus the
/// zero-valued change output every vault spend carries.
pub fn fee(notes: usize) -> u64 {
    MARGINAL_FEE * GRACE_ACTIONS.max((notes as u64).max(2))
}

/// The one payment the vault makes: (address, amount). `held` are the values
/// of the notes the vault holds.
pub fn payout(terms: &EscrowTerms, held: &[u64], to: Release) -> Option<(String, u64)> {
    let total: u64 = held.iter().sum();
    let fee = fee(held.len());
    if total <= fee {
        return None;
    }
    let addr = match to {
        Release::ToSeller => terms.seller.clone(),
        Release::ToBuyer => terms.buyer.clone(),
    };
    Some((addr, total - fee))
}

/// Whether an unattended arbiter should cosign `to`, requested by seat
/// `requester`. Disputes (anything else) need a human.
pub fn arbiter_auto_approves(requester: u16, to: Release) -> bool {
    matches!((requester, to), (BUYER, Release::ToSeller) | (SELLER, Release::ToBuyer))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms() -> EscrowTerms {
        EscrowTerms {
            network: "regtest".into(),
            vault_ufvk: "uview1...".into(),
            seats: vec!["11".repeat(32), "22".repeat(32), "33".repeat(32)],
            buyer: "u1buyer".into(),
            seller: "u1seller".into(),
            item: "a used bike".into(),
            amount: 50_000_000,
            created_height: 100,
        }
    }

    #[test]
    fn rules() {
        let t = terms();
        assert!(t.validate().is_ok());
        assert_eq!(payout(&t, &[50_000_000], Release::ToSeller), Some(("u1seller".into(), 50_000_000 - 10_000)));
        assert_eq!(payout(&t, &[30_000_000, 20_000_000, 1], Release::ToBuyer).unwrap().1, 50_000_001 - 15_000);
        assert!(arbiter_auto_approves(BUYER, Release::ToSeller));
        assert!(arbiter_auto_approves(SELLER, Release::ToBuyer));
        assert!(!arbiter_auto_approves(BUYER, Release::ToBuyer));
        assert!(!arbiter_auto_approves(SELLER, Release::ToSeller));
        assert_eq!(t.seat_of(&"22".repeat(32)), Some(SELLER));
        let mut other = t.clone();
        other.amount += 1;
        assert_ne!(t.id(), other.id());
    }
}
