//! Building, proving and signing a fully shielded Ironwood payment.
//!
//! The pipeline is the same one `tacit-wallet` runs through librustzcash
//! (builder -> PCZT -> proof -> signatures -> transaction), minus the
//! database: the notes and witnesses come straight from a [`Scanner`].

use orchard::circuit::{OrchardCircuitVersion, ProvingKey};
use pczt::roles::creator::Creator;
use pczt::roles::io_finalizer::IoFinalizer;
use pczt::roles::prover::Prover;
use pczt::roles::signer::Signer;
use pczt::roles::tx_extractor::TransactionExtractor;
use rand_core::OsRng;
use tacit_core::auction::Terms;
use tacit_core::memo::{Bid, SealedBid};
use zcash_primitives::transaction::builder::{BuildConfig, Builder, BundlePadding};
use zcash_primitives::transaction::fees::zip317;
use zcash_protocol::consensus::BlockHeight;
use zcash_protocol::memo::MemoBytes;
use zcash_protocol::value::Zatoshis;

use crate::keys::{decode_address, decode_fvk, vault_address, Account};
use crate::{invalid, Error, Net, Result, Scanner};

/// Proving key for the post NU6.3 circuit. Takes several seconds to build,
/// so build it once and keep it.
pub fn proving_key() -> ProvingKey {
    ProvingKey::build(OrchardCircuitVersion::PostNu6_3)
}

/// ZIP 317 fee for an Ironwood-only transaction with this many spends and
/// outputs.
pub fn fee(spends: usize, outputs: usize) -> u64 {
    5_000 * spends.max(outputs).max(2) as u64
}

/// One output of a payment.
pub struct Output {
    pub to: orchard::Address,
    pub value: u64,
    pub memo: Option<[u8; 512]>,
}

/// A finished transaction, ready to broadcast.
pub struct Built {
    /// Display-order txid.
    pub txid: String,
    pub data: Vec<u8>,
    pub fee: u64,
    /// Nullifiers of the notes it spends, to mark them pending.
    pub spent: Vec<[u8; 32]>,
}

/// Pays `outputs` from the scanner's notes, sending change back to the
/// account. Valid at the height right after the last block scanned.
pub fn pay(account: &Account, scanner: &Scanner, pk: &ProvingKey, outputs: &[Output]) -> Result<Built> {
    if outputs.is_empty() {
        return Err(invalid("nothing to pay"));
    }
    let want: u64 = outputs.iter().map(|o| o.value).sum();

    // Largest notes first keeps the number of spends (and the fee) low.
    let mut notes: Vec<_> = scanner
        .found()
        .iter()
        .filter(|f| f.is_unspent() && f.pending.is_none() && f.value() > 0)
        .collect();
    notes.sort_by(|a, b| b.value().cmp(&a.value()));
    let mut chosen = Vec::new();
    let mut have = 0;
    for n in notes {
        if have >= want + fee(chosen.len(), outputs.len() + 1) {
            break;
        }
        have += n.value();
        chosen.push(n);
    }
    let fee = fee(chosen.len(), outputs.len() + 1);
    if have < want + fee {
        return Err(Error::Funds { have: scanner.spendable(), need: want + fee });
    }
    let change = Output { to: account.change_address(), value: have - want - fee, memo: None };
    let (pczt, spends) = unsigned(scanner, pk, &chosen, outputs, &change)?;
    let mut signer = Signer::new(pczt).map_err(|e| Error::Build(format!("{e:?}")))?;
    let ask = account.ask();
    for i in spends {
        signer.sign_ironwood(i, &ask).map_err(|e| Error::Build(format!("signing: {e:?}")))?;
    }
    let mut built = extract(signer.finish())?;
    built.fee = fee;
    built.spent = chosen.iter().map(|n| n.nullifier).collect();
    Ok(built)
}

/// A proven but unsigned transaction that empties a vault into one payment
/// of everything it holds minus the fee, plus the zero-valued change note
/// every vault spend carries. Its spends get their signatures from FROST.
/// Returns the PCZT and the amount paid.
pub fn vault_payout(scanner: &Scanner, pk: &ProvingKey, to: orchard::Address) -> Result<(pczt::Pczt, u64)> {
    let notes: Vec<_> = scanner.found().iter().filter(|f| f.is_unspent() && f.value() > 0).collect();
    let total: u64 = notes.iter().map(|n| n.value()).sum();
    let fee = fee(notes.len(), 2);
    if total <= fee {
        return Err(Error::Funds { have: total, need: fee + 1 });
    }
    let pay = Output { to, value: total - fee, memo: None };
    let change = Output { to: scanner.fvk().address_at(0u32, orchard::keys::Scope::Internal), value: 0, memo: None };
    let (pczt, _) = unsigned(scanner, pk, &notes, std::slice::from_ref(&pay), &change)?;
    Ok((pczt, total - fee))
}

/// Builds and proves a transaction spending `notes` into `outputs` and
/// `change`. Returns the PCZT and the action index of each real spend.
fn unsigned(
    scanner: &Scanner,
    pk: &ProvingKey,
    notes: &[&crate::scan::Found],
    outputs: &[Output],
    change: &Output,
) -> Result<(pczt::Pczt, Vec<usize>)> {
    let net: Net = scanner.net();
    let fvk = scanner.fvk();
    let target = BlockHeight::from_u32(scanner.height() + 1);
    let mut builder = Builder::new(
        net,
        target,
        BuildConfig::Standard {
            sapling_anchor: None,
            orchard_anchor: None,
            ironwood_anchor: Some(scanner.anchor()),
            orchard_padding: BundlePadding::DEFAULT,
            ironwood_padding: BundlePadding::DEFAULT,
        },
    );
    let build = |e: zcash_primitives::transaction::builder::Error<_>| Error::Build(format!("{e:?}"));
    for n in notes {
        let path = n.merkle_path().ok_or_else(|| Error::Build("note has no witness yet".into()))?;
        builder.add_ironwood_spend::<zip317::FeeError>(fvk.clone(), n.note, path).map_err(build)?;
    }
    let ovk = Some(fvk.to_ovk(orchard::keys::Scope::External));
    for o in outputs.iter().chain(std::iter::once(change)) {
        let memo = match o.memo {
            Some(m) => MemoBytes::from_bytes(&m).map_err(|e| Error::Build(format!("{e:?}")))?,
            None => MemoBytes::empty(),
        };
        let value = Zatoshis::from_u64(o.value).map_err(|_| invalid("amount out of range"))?;
        builder.add_ironwood_output::<zip317::FeeError>(ovk.clone(), o.to, value, memo).map_err(build)?;
    }
    let result = builder.build_for_pczt(OsRng, &zip317::FeeRule::standard()).map_err(build)?;
    let spends: Vec<usize> = (0..notes.len())
        .map(|i| result.ironwood_meta.spend_action_index(i).expect("one action per spend"))
        .collect();
    let pczt = Creator::build_from_parts(result.pczt_parts).ok_or_else(|| Error::Build("can't make a PCZT".into()))?;
    let pczt = IoFinalizer::new(pczt).finalize_io().map_err(|e| Error::Build(format!("{e:?}")))?;
    let pczt = Prover::new(pczt)
        .create_ironwood_proof(pk)
        .map_err(|e| Error::Build(format!("proving: {e:?}")))?
        .finish();
    Ok((pczt, spends))
}

/// Turns a fully authorized PCZT into a transaction ready to broadcast.
pub fn extract(pczt: pczt::Pczt) -> Result<Built> {
    let tx = TransactionExtractor::new(pczt).extract().map_err(|e| Error::Build(format!("{e:?}")))?;
    let mut data = Vec::new();
    tx.write(&mut data).map_err(|e| Error::Build(e.to_string()))?;
    Ok(Built { txid: tx.txid().to_string(), data, fee: 0, spent: vec![] })
}

/// Seals a bid of `amount` zatoshi and builds the deposit of `terms.bond`
/// into the auction's vault. Refunds go to the account's address number
/// `refund_index`, which should be one the account hasn't handed out before.
pub fn bid(
    account: &Account,
    scanner: &Scanner,
    pk: &ProvingKey,
    terms: &Terms,
    amount: u64,
    refund_index: u32,
) -> Result<Built> {
    terms.validate().map_err(invalid)?;
    let net = Net::parse(&terms.network).ok_or_else(|| invalid("unknown network in terms"))?;
    if net != account.net() {
        return Err(invalid(format!("this auction runs on {}, the wallet on {}", net.name(), account.net().name())));
    }
    if amount < terms.reserve || amount > terms.bond {
        return Err(invalid(format!(
            "a bid must be between the reserve ({} zat) and the bond ({} zat)",
            terms.reserve, terms.bond
        )));
    }
    let vault = vault_address(&decode_fvk(net, &terms.vault_ufvk)?);
    let bid = Bid { auction: terms.id(), amount, refund_to: account.address(refund_index).to_raw_address_bytes() };
    let memo = SealedBid::seal(&bid, terms.close_round).map_err(invalid)?.to_memo();
    pay(account, scanner, pk, &[Output { to: vault, value: terms.bond, memo: Some(memo) }])
}

/// A plain shielded send to a unified address.
pub fn send(account: &Account, scanner: &Scanner, pk: &ProvingKey, to: &str, value: u64, memo: Option<[u8; 512]>) -> Result<Built> {
    let to = decode_address(account.net(), to)?;
    pay(account, scanner, pk, &[Output { to, value, memo }])
}
