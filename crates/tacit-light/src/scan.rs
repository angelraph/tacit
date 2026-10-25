//! Finding our notes in compact blocks and keeping them spendable.
//!
//! Hashing the note commitment tree is what costs time in a browser, and a
//! busy chain carries hundreds of commitments a block. So the scanner only
//! hashes what it must: an incremental witness for each unspent note, and
//! the tree's frontier only in a block that pays us, fetched from the server
//! for that block ([`Error::NeedTree`]) and dropped afterwards. Everything
//! else is trial decryption. An auditor keeps no witnesses at all.

use incrementalmerkletree::frontier::CommitmentTree;
use incrementalmerkletree::witness::IncrementalWitness;
use orchard::keys::{FullViewingKey, PreparedIncomingViewingKey, Scope};
use orchard::note::{ExtractedNoteCommitment, Nullifier, RandomSeed, Rho};
use orchard::note_encryption::{CompactAction, IronwoodDomain};
use orchard::tree::{MerkleHashOrchard, MerklePath};
use orchard::value::NoteValue;
use orchard::Note;
use serde::{Deserialize, Serialize};
use zcash_note_encryption::{try_compact_note_decryption, EphemeralKeyBytes};
use zcash_primitives::merkle_tree::{
    read_commitment_tree, read_incremental_witness, write_commitment_tree, write_incremental_witness,
};

use crate::chain::{self, Block};
use crate::{hex32, invalid, Error, Net, Result};

pub type Tree = CommitmentTree<MerkleHashOrchard, 32>;
pub type Witness = IncrementalWitness<MerkleHashOrchard, 32>;

/// A note the scanner found.
#[derive(Clone, Debug)]
pub struct Found {
    /// Display-order txid of the transaction that created it.
    pub txid: String,
    /// Index of the action in that transaction's Ironwood bundle.
    pub action: u32,
    pub height: u32,
    pub note: Note,
    /// Whether it was sent to an internal (change) address.
    pub change: bool,
    pub nullifier: [u8; 32],
    /// None for a scanner that only watches (an audit), or once spent.
    pub witness: Option<Witness>,
    /// Display-order txid of the transaction that spent it, once seen.
    pub spent_in: Option<String>,
    /// Txid of a transaction we built that spends it but that hasn't been
    /// mined yet.
    pub pending: Option<String>,
}

impl Found {
    pub fn value(&self) -> u64 {
        self.note.value().inner()
    }

    /// The deposit id the auction rules use: internal-order txid followed by
    /// the action index as a little endian u32.
    pub fn deposit_id(&self) -> Vec<u8> {
        let mut id = chain::txid_bytes(&self.txid).expect("stored txids are valid").to_vec();
        id.extend_from_slice(&self.action.to_le_bytes());
        id
    }

    pub fn is_unspent(&self) -> bool {
        self.spent_in.is_none()
    }

    pub fn merkle_path(&self) -> Option<MerklePath> {
        self.witness.as_ref().and_then(|w| w.path()).map(MerklePath::from)
    }
}

/// One action of a block, parsed once.
struct Parsed {
    nf: [u8; 32],
    node: MerkleHashOrchard,
    /// Our note, if trial decryption found one, and whether it's change.
    ours: Option<(Note, bool)>,
}

/// Scans the chain with one viewing key.
pub struct Scanner {
    net: Net,
    fvk: FullViewingKey,
    ivks: [(PreparedIncomingViewingKey, bool); 2],
    /// Height of the last block scanned.
    height: u32,
    /// Hash of that block, to notice reorganisations.
    hash: String,
    /// The frontier, only while a block that pays us needs it.
    tree: Option<Tree>,
    /// Leaves in the tree after the last block scanned.
    size: u64,
    /// False for an auditor, which never spends and so needs no witnesses.
    witnesses: bool,
    found: Vec<Found>,
    /// Every transaction that spent one of our notes, in chain order.
    spends: Vec<(String, u32)>,
}

impl Scanner {
    /// Starts scanning after `birthday`, the tree state at the block before
    /// the first one that can hold our notes.
    pub fn new(net: Net, fvk: FullViewingKey, birthday: &chain::Tree) -> Result<Self> {
        let tree = parse_tree(&birthday.ironwood_tree)?;
        let ivks = [
            (fvk.to_ivk(Scope::External).prepare(), false),
            (fvk.to_ivk(Scope::Internal).prepare(), true),
        ];
        Ok(Scanner {
            net,
            fvk,
            ivks,
            height: birthday.height as u32,
            hash: birthday.hash.clone(),
            size: tree.size() as u64,
            tree: Some(tree),
            witnesses: true,
            found: vec![],
            spends: vec![],
        })
    }

    /// A scanner that only watches: it finds notes and their spends but
    /// keeps no witnesses, so it never hashes the tree.
    pub fn watch_only(net: Net, fvk: FullViewingKey, birthday: &chain::Tree) -> Result<Self> {
        let mut s = Self::new(net, fvk, birthday)?;
        s.witnesses = false;
        s.tree = None;
        Ok(s)
    }

    /// Supplies the tree state after the last scanned block, which
    /// [`Error::NeedTree`] asked for. Scanning then continues where it was.
    pub fn set_tree(&mut self, t: &chain::Tree) -> Result<()> {
        if t.height as u32 != self.height {
            return Err(Error::Chain(format!("tree state is for block {}, need {}", t.height, self.height)));
        }
        if !self.hash.is_empty() && !t.hash.is_empty() && t.hash != self.hash {
            return Err(Error::Reorg(self.height));
        }
        let tree = parse_tree(&t.ironwood_tree)?;
        if tree.size() as u64 != self.size {
            return Err(Error::Chain(format!(
                "tree state has {} leaves, the scan counted {}",
                tree.size(),
                self.size
            )));
        }
        self.tree = Some(tree);
        Ok(())
    }

    pub fn net(&self) -> Net {
        self.net
    }

    pub fn fvk(&self) -> &FullViewingKey {
        &self.fvk
    }

    /// Height of the last block scanned.
    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn found(&self) -> &[Found] {
        &self.found
    }

    pub fn spends(&self) -> &[(String, u32)] {
        &self.spends
    }

    /// The anchor our witnesses currently prove membership against. Every
    /// live witness has seen the same leaves, so any of them gives the root.
    pub fn anchor(&self) -> orchard::Anchor {
        match self.found.iter().find_map(|f| f.witness.as_ref()) {
            Some(w) => w.root().into(),
            None => self.tree.as_ref().map(|t| t.root()).unwrap_or_else(|| Tree::empty().root()).into(),
        }
    }

    /// Value of unspent notes that aren't waiting on a pending spend.
    pub fn spendable(&self) -> u64 {
        self.found.iter().filter(|f| f.is_unspent() && f.pending.is_none()).map(Found::value).sum()
    }

    /// Value of unspent notes, including ones a pending transaction spends.
    pub fn balance(&self) -> u64 {
        self.found.iter().filter(|f| f.is_unspent()).map(Found::value).sum()
    }

    /// Marks notes as spent by a transaction we just broadcast.
    pub fn mark_pending(&mut self, nullifiers: &[[u8; 32]], txid: &str) {
        for f in &mut self.found {
            if nullifiers.contains(&f.nullifier) {
                f.pending = Some(txid.to_string());
            }
        }
    }

    /// Forgets pending spends that never got mined (say, they expired).
    pub fn clear_pending(&mut self) {
        for f in &mut self.found {
            f.pending = None;
        }
    }

    /// Scans consecutive blocks starting right after the last one scanned.
    pub fn scan(&mut self, blocks: &[Block]) -> Result<()> {
        for b in blocks {
            self.scan_block(b)?;
        }
        Ok(())
    }

    fn scan_block(&mut self, b: &Block) -> Result<()> {
        if b.height as u32 != self.height + 1 {
            return Err(Error::Chain(format!("expected block {}, got {}", self.height + 1, b.height)));
        }
        if !self.hash.is_empty() && b.prev_hash != self.hash {
            return Err(Error::Reorg(self.height));
        }

        // Parse and trial-decrypt the whole block first, without changing
        // anything, so a block that pays us can stop and ask for the tree.
        let mut parsed: Vec<Vec<Parsed>> = Vec::with_capacity(b.txs.len());
        let mut pays_us = false;
        for tx in &b.txs {
            let mut actions = Vec::with_capacity(tx.actions.len());
            for a in &tx.actions {
                let nf = hex32(&a.nf)?;
                let cmx = Option::from(ExtractedNoteCommitment::from_bytes(&hex32(&a.cmx)?))
                    .ok_or_else(|| Error::Chain("bad note commitment".into()))?;
                let ct: [u8; 52] = hex::decode(&a.ct)
                    .ok()
                    .and_then(|v| v.get(..52).and_then(|s| s.try_into().ok()))
                    .ok_or_else(|| Error::Chain("bad compact ciphertext".into()))?;
                let nullifier = Option::from(Nullifier::from_bytes(&nf))
                    .ok_or_else(|| Error::Chain("bad nullifier".into()))?;
                let action = CompactAction::from_parts(nullifier, cmx, EphemeralKeyBytes(hex32(&a.epk)?), ct);
                let domain = IronwoodDomain::for_compact_action(&action);
                let ours = self.ivks.iter().find_map(|(ivk, change)| {
                    try_compact_note_decryption(&domain, ivk, &action).map(|(note, _)| (note, *change))
                });
                pays_us |= ours.is_some();
                actions.push(Parsed { nf, node: MerkleHashOrchard::from_cmx(&cmx), ours });
            }
            parsed.push(actions);
        }
        if pays_us && self.witnesses && self.tree.is_none() {
            return Err(Error::NeedTree(self.height));
        }

        for (tx, actions) in b.txs.iter().zip(parsed) {
            for (i, p) in actions.into_iter().enumerate() {
                let mut spent_ours = false;
                for f in &mut self.found {
                    if f.nullifier == p.nf {
                        f.spent_in = Some(tx.txid.clone());
                        f.pending = None;
                        // A spent note never needs its witness again.
                        f.witness = None;
                        spent_ours = true;
                    }
                }
                if spent_ours && self.spends.last().map(|s| &s.0) != Some(&tx.txid) {
                    self.spends.push((tx.txid.clone(), b.height as u32));
                }

                if let Some(t) = &mut self.tree {
                    t.append(p.node).map_err(|_| Error::Chain("note commitment tree is full".into()))?;
                }
                self.size += 1;
                for w in self.found.iter_mut().filter_map(|f| f.witness.as_mut()) {
                    w.append(p.node).map_err(|_| Error::Chain("witness is full".into()))?;
                }

                if let Some((note, change)) = p.ours {
                    let witness = match (&self.tree, self.witnesses) {
                        (Some(t), true) => Some(
                            Witness::from_tree(t.clone()).ok_or_else(|| Error::Chain("empty tree after append".into()))?,
                        ),
                        _ => None,
                    };
                    self.found.push(Found {
                        txid: tx.txid.clone(),
                        action: i as u32,
                        height: b.height as u32,
                        nullifier: note.nullifier(&self.fvk).to_bytes(),
                        note,
                        change,
                        witness,
                        spent_in: None,
                        pending: None,
                    });
                }
            }
        }
        if b.ironwood_tree_size != 0 && self.size != b.ironwood_tree_size as u64 {
            return Err(Error::Chain(format!(
                "note commitment tree has {} leaves after block {}, the server says {}",
                self.size, b.height, b.ironwood_tree_size
            )));
        }
        // The frontier was only for this block's notes.
        self.tree = None;
        self.height = b.height as u32;
        self.hash = b.hash.clone();
        Ok(())
    }

    /// Serialises everything the scanner knows, so a page reload can resume.
    pub fn save(&self) -> Saved {
        Saved {
            net: self.net,
            fvk: hex::encode(self.fvk.to_bytes()),
            height: self.height,
            hash: self.hash.clone(),
            tree: self.tree.as_ref().map(|t| hex::encode(tree_bytes(t))).unwrap_or_default(),
            size: self.size,
            witnesses: self.witnesses,
            notes: self
                .found
                .iter()
                .map(|f| SavedNote {
                    txid: f.txid.clone(),
                    action: f.action,
                    height: f.height,
                    recipient: hex::encode(f.note.recipient().to_raw_address_bytes()),
                    value: f.value(),
                    rho: hex::encode(f.note.rho().to_bytes()),
                    rseed: hex::encode(f.note.rseed().as_bytes()),
                    change: f.change,
                    witness: f.witness.as_ref().map(|w| hex::encode(witness_bytes(w))).unwrap_or_default(),
                    spent_in: f.spent_in.clone(),
                    pending: f.pending.clone(),
                })
                .collect(),
            spends: self.spends.clone(),
        }
    }

    pub fn load(s: &Saved) -> Result<Self> {
        let fvk_bytes: [u8; 96] = hex::decode(&s.fvk).ok().and_then(|b| b.try_into().ok()).ok_or_else(|| invalid("bad saved viewing key"))?;
        let fvk = FullViewingKey::from_bytes(&fvk_bytes).ok_or_else(|| invalid("bad saved viewing key"))?;
        // Saves from before lazy trees always carry the tree; newer ones
        // carry the leaf count instead.
        let tree: Option<Tree> = if s.tree.is_empty() {
            None
        } else {
            Some(read_commitment_tree(&hex::decode(&s.tree).map_err(invalid)?[..]).map_err(invalid)?)
        };
        let size = tree.as_ref().map(|t| t.size() as u64).unwrap_or(s.size);
        let mut found = Vec::new();
        for n in &s.notes {
            let raw: [u8; 43] = hex::decode(&n.recipient).ok().and_then(|b| b.try_into().ok()).ok_or_else(|| invalid("bad saved note"))?;
            let recipient = Option::from(orchard::Address::from_raw_address_bytes(&raw)).ok_or_else(|| invalid("bad saved note"))?;
            let rho = Option::from(Rho::from_bytes(&hex32(&n.rho)?)).ok_or_else(|| invalid("bad saved note"))?;
            let rseed = Option::from(RandomSeed::from_bytes(hex32(&n.rseed)?, &rho)).ok_or_else(|| invalid("bad saved note"))?;
            let note: Note = Option::from(Note::from_parts(recipient, NoteValue::from_raw(n.value), rho, rseed, orchard::note::NoteVersion::V3))
                .ok_or_else(|| invalid("bad saved note"))?;
            let witness = if n.witness.is_empty() || n.spent_in.is_some() {
                None
            } else {
                Some(read_incremental_witness(&hex::decode(&n.witness).map_err(invalid)?[..]).map_err(invalid)?)
            };
            found.push(Found {
                txid: n.txid.clone(),
                action: n.action,
                height: n.height,
                nullifier: note.nullifier(&fvk).to_bytes(),
                note,
                change: n.change,
                witness,
                spent_in: n.spent_in.clone(),
                pending: n.pending.clone(),
            });
        }
        let ivks = [
            (fvk.to_ivk(Scope::External).prepare(), false),
            (fvk.to_ivk(Scope::Internal).prepare(), true),
        ];
        Ok(Scanner {
            net: s.net,
            fvk,
            ivks,
            height: s.height,
            hash: s.hash.clone(),
            tree,
            size,
            witnesses: s.witnesses,
            found,
            spends: s.spends.clone(),
        })
    }
}

/// A scanner's state as JSON-friendly data.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Saved {
    pub net: Net,
    pub fvk: String,
    pub height: u32,
    pub hash: String,
    /// Empty unless a block was mid-scan; [`Saved::size`] says where the tree is.
    #[serde(default)]
    pub tree: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default = "yes")]
    pub witnesses: bool,
    pub notes: Vec<SavedNote>,
    #[serde(default)]
    pub spends: Vec<(String, u32)>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedNote {
    pub txid: String,
    pub action: u32,
    pub height: u32,
    pub recipient: String,
    pub value: u64,
    pub rho: String,
    pub rseed: String,
    pub change: bool,
    pub witness: String,
    pub spent_in: Option<String>,
    pub pending: Option<String>,
}

fn yes() -> bool {
    true
}

/// Parses lightwalletd's hex tree state (empty for an empty tree).
pub fn parse_tree(hex_tree: &str) -> Result<Tree> {
    if hex_tree.is_empty() {
        return Ok(Tree::empty());
    }
    let bytes = hex::decode(hex_tree).map_err(|_| Error::Chain("tree state is not hex".into()))?;
    read_commitment_tree(&bytes[..]).map_err(|e| Error::Chain(format!("bad tree state: {e}")))
}

fn tree_bytes(t: &Tree) -> Vec<u8> {
    let mut v = Vec::new();
    write_commitment_tree(t, &mut v).expect("writing to a Vec can't fail");
    v
}

fn witness_bytes(w: &Witness) -> Vec<u8> {
    let mut v = Vec::new();
    write_incremental_witness(w, &mut v).expect("writing to a Vec can't fail");
    v
}
