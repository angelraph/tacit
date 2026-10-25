//! Tacit's memo format.
//!
//! Every Tacit memo starts with 0xF5, which ZIP 302 reserves for arbitrary
//! binary data, so wallets that don't know Tacit show it as data rather than
//! trying to render text. Then comes a short magic, a version and a kind.
//!
//! ```text
//! 0xF5 | "TACIT" | version (1) | kind (1) | body ... | zero padding to 512
//! ```
//!
//! A sealed bid's body is the auction id in the clear (so auditors can find
//! every bid for an auction) followed by a timelocked box. Inside the box the
//! auction id is repeated, which stops anyone from lifting a sealed bid out of
//! one auction and replaying it into another.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};

use crate::seal::{self, SealError};

pub const MEMO_LEN: usize = 512;
const PREFIX: [u8; 6] = [0xF5, b'T', b'A', b'C', b'I', b'T'];
const VERSION: u8 = 1;

const KIND_SEALED_BID: u8 = 1;

/// Length of a raw Orchard-protocol payment address (diversifier + pk_d),
/// which is also how Ironwood receivers are encoded.
pub const RAW_ADDRESS_LEN: usize = 43;

pub type AuctionId = [u8; 32];

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MemoError {
    #[error("not a Tacit memo")]
    NotTacit,
    #[error("unsupported Tacit memo version {0}")]
    Version(u8),
    #[error("unknown Tacit memo kind {0}")]
    Kind(u8),
    #[error("memo is truncated")]
    Truncated,
    #[error("sealed bid names a different auction inside than outside")]
    AuctionMismatch,
    #[error("seal: {0}")]
    Seal(String),
}

impl From<SealError> for MemoError {
    fn from(e: SealError) -> Self {
        MemoError::Seal(e.to_string())
    }
}

/// What a bidder commits to, readable only after the auction closes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bid {
    pub auction: AuctionId,
    /// Bid amount in zatoshi. Must not exceed the bond it rides on.
    pub amount: u64,
    /// Fresh diversified address for the refund or change.
    pub refund_to: [u8; RAW_ADDRESS_LEN],
}

const BID_PLAINTEXT_LEN: usize = 1 + 32 + 8 + RAW_ADDRESS_LEN;

impl Bid {
    fn to_bytes(&self) -> [u8; BID_PLAINTEXT_LEN] {
        let mut out = [0u8; BID_PLAINTEXT_LEN];
        out[0] = VERSION;
        out[1..33].copy_from_slice(&self.auction);
        out[33..41].copy_from_slice(&self.amount.to_be_bytes());
        out[41..].copy_from_slice(&self.refund_to);
        out
    }

    fn from_bytes(b: &[u8]) -> Result<Self, MemoError> {
        if b.len() != BID_PLAINTEXT_LEN {
            return Err(MemoError::Truncated);
        }
        if b[0] != VERSION {
            return Err(MemoError::Version(b[0]));
        }
        Ok(Bid {
            auction: b[1..33].try_into().unwrap(),
            amount: u64::from_be_bytes(b[33..41].try_into().unwrap()),
            refund_to: b[41..].try_into().unwrap(),
        })
    }
}

/// A sealed bid as it appears on chain, before the auction closes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealedBid {
    pub auction: AuctionId,
    sealed: Vec<u8>,
}

impl SealedBid {
    pub fn seal(bid: &Bid, close_round: u64) -> Result<Self, MemoError> {
        Ok(SealedBid { auction: bid.auction, sealed: seal::seal(&bid.to_bytes(), close_round)? })
    }

    /// drand round this bid unlocks at.
    pub fn close_round(&self) -> u64 {
        seal::target_round(&self.sealed).expect("constructed boxes always have a header")
    }

    pub fn open(&self, beacon: &crate::beacon::Beacon) -> Result<Bid, MemoError> {
        let bid = Bid::from_bytes(&seal::open(&self.sealed, beacon)?)?;
        if bid.auction != self.auction {
            return Err(MemoError::AuctionMismatch);
        }
        Ok(bid)
    }

    pub fn to_memo(&self) -> [u8; MEMO_LEN] {
        let mut memo = [0u8; MEMO_LEN];
        let mut w = Writer::new(&mut memo);
        w.put(&PREFIX);
        w.put(&[VERSION, KIND_SEALED_BID]);
        w.put(&self.auction);
        w.put(&(self.sealed.len() as u16).to_be_bytes());
        w.put(&self.sealed);
        memo
    }

    /// The same memo as printable text, for wallets whose send API only
    /// takes a UTF-8 memo (browser extensions such as Noir). About 380 bytes,
    /// well inside the 512 a memo holds.
    pub fn to_text_memo(&self) -> String {
        let memo = self.to_memo();
        let used = PREFIX.len() + 2 + 32 + 2 + self.sealed.len();
        format!("{TEXT_PREFIX}{}", URL_SAFE_NO_PAD.encode(&memo[..used]))
    }
}

/// Marks the text form of a Tacit memo: this prefix, then base64url of the
/// binary memo without its zero padding.
const TEXT_PREFIX: &str = "TACIT1:";

/// Anything Tacit writes into a memo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TacitMemo {
    SealedBid(SealedBid),
}

impl TacitMemo {
    pub fn parse(memo: &[u8]) -> Result<Self, MemoError> {
        if let Some(text) = memo.strip_prefix(TEXT_PREFIX.as_bytes()) {
            let end = text.iter().position(|&b| b == 0).unwrap_or(text.len());
            let binary = URL_SAFE_NO_PAD.decode(&text[..end]).map_err(|_| MemoError::Truncated)?;
            if binary.first() != Some(&0xF5) {
                return Err(MemoError::NotTacit);
            }
            return Self::parse(&binary);
        }
        let mut r = Reader(memo);
        if r.take(PREFIX.len())? != PREFIX {
            return Err(MemoError::NotTacit);
        }
        let version = r.byte()?;
        if version != VERSION {
            return Err(MemoError::Version(version));
        }
        match r.byte()? {
            KIND_SEALED_BID => {
                let auction = r.take(32)?.try_into().unwrap();
                let len = u16::from_be_bytes(r.take(2)?.try_into().unwrap()) as usize;
                let sealed = r.take(len)?.to_vec();
                if sealed.len() != seal::sealed_len(BID_PLAINTEXT_LEN) {
                    return Err(MemoError::Truncated);
                }
                Ok(TacitMemo::SealedBid(SealedBid { auction, sealed }))
            }
            k => Err(MemoError::Kind(k)),
        }
    }
}

struct Writer<'a> {
    buf: &'a mut [u8],
    at: usize,
}

impl<'a> Writer<'a> {
    fn new(buf: &'a mut [u8]) -> Self {
        Writer { buf, at: 0 }
    }
    fn put(&mut self, bytes: &[u8]) {
        self.buf[self.at..self.at + bytes.len()].copy_from_slice(bytes);
        self.at += bytes.len();
    }
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], MemoError> {
        if self.0.len() < n {
            return Err(MemoError::Truncated);
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }
    fn byte(&mut self) -> Result<u8, MemoError> {
        Ok(self.take(1)?[0])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::beacon::Beacon;

    const ROUND: u64 = 32_633_680;
    const SIG: &str = "980de8271fc02d5191a0e8977922c347f5dbad595e2d080117ab64385a341b79cfb7ebef0459b5260209dc77259d8121";

    fn beacon() -> Beacon {
        Beacon::verify(ROUND, &hex::decode(SIG).unwrap()).unwrap()
    }

    fn bid() -> Bid {
        Bid { auction: [9; 32], amount: 3_1415_9265, refund_to: [7; RAW_ADDRESS_LEN] }
    }

    #[test]
    fn sealed_bid_round_trips_through_a_memo() {
        let sealed = SealedBid::seal(&bid(), ROUND).unwrap();
        let memo = sealed.to_memo();
        assert_eq!(memo[0], 0xF5, "ZIP 302 arbitrary data marker");
        let TacitMemo::SealedBid(parsed) = TacitMemo::parse(&memo).unwrap();
        assert_eq!(parsed.auction, [9; 32]);
        assert_eq!(parsed.close_round(), ROUND);
        assert_eq!(parsed.open(&beacon()).unwrap(), bid());
    }

    #[test]
    fn the_text_form_parses_like_the_binary_one() {
        let sealed = SealedBid::seal(&bid(), ROUND).unwrap();
        let text = sealed.to_text_memo();
        assert!(text.len() <= MEMO_LEN, "{} bytes", text.len());
        let mut memo = [0u8; MEMO_LEN];
        memo[..text.len()].copy_from_slice(text.as_bytes());
        let TacitMemo::SealedBid(parsed) = TacitMemo::parse(&memo).unwrap();
        assert_eq!(parsed.open(&beacon()).unwrap(), bid());
    }

    #[test]
    fn a_bid_moved_to_another_auction_is_rejected() {
        let mut sealed = SealedBid::seal(&bid(), ROUND).unwrap();
        sealed.auction = [1; 32];
        let TacitMemo::SealedBid(parsed) = TacitMemo::parse(&sealed.to_memo()).unwrap();
        assert_eq!(parsed.open(&beacon()), Err(MemoError::AuctionMismatch));
    }

    #[test]
    fn ignores_ordinary_memos() {
        let mut text = [0u8; MEMO_LEN];
        text[..5].copy_from_slice(b"hello");
        assert_eq!(TacitMemo::parse(&text), Err(MemoError::NotTacit));
        assert_eq!(TacitMemo::parse(&[0xF5, b'T']), Err(MemoError::Truncated));
    }
}
