//! A small Ironwood light wallet that runs the same natively and in a browser.
//!
//! Tacit's main wallet (`tacit-wallet`) sits on librustzcash's SQLite backend,
//! which a browser can't run. This crate is the part a bidder or an auditor
//! needs, written against plain data so it compiles to WebAssembly:
//!
//! - [`keys`]: an account from a seed, its addresses and viewing keys
//! - [`scan`]: trial-decrypting compact blocks and keeping note witnesses
//! - [`build`]: building, proving and signing an Ironwood payment
//! - [`audit`]: rebuilding an auction from its published terms alone
//!
//! Chain data comes from a coordinator's chain gateway as JSON ([`chain`]).
//! Nothing secret ever leaves the wallet: blocks come in, and only finished,
//! fully shielded transactions go out.

pub mod audit;
pub mod build;
pub mod chain;
pub mod keys;
pub mod net;
pub mod scan;

pub use keys::Account;
pub use net::Net;
pub use scan::Scanner;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Invalid(String),
    #[error("chain data: {0}")]
    Chain(String),
    #[error("the chain reorganised below height {0}; rescan from an earlier height")]
    Reorg(u32),
    /// The next block pays us, so the scanner needs the tree state after
    /// this height ([`Scanner::set_tree`]) before it can build a witness.
    #[error("need the tree state at block {0}")]
    NeedTree(u32),
    #[error("not enough funds: have {have} zat spendable, need {need}")]
    Funds { have: u64, need: u64 },
    #[error("building transaction: {0}")]
    Build(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn invalid(e: impl std::fmt::Display) -> Error {
    Error::Invalid(e.to_string())
}

pub(crate) fn hex32(s: &str) -> Result<[u8; 32]> {
    hex::decode(s)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| Error::Chain(format!("expected 32 hex bytes, got {s:?}")))
}
