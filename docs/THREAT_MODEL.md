# Tacit Threat Model

> The price is public. You are not.

This document says exactly what Tacit hides, from whom, and when, and where it does **not** protect you.
If any claim here doesn't hold in the code, that is a bug. Please report it.

## Actors

| Actor | Can see | Cannot see |
|---|---|---|
| Public chain observer | That Ironwood transactions happened, plus their fees | Senders, recipients, amounts, memos |
| Auction auditor (holds the published vault IVK/OVK) | Every bond amount and every sealed-bid memo reaching the vault; every settlement output | Who sent any bond; which wallet owns any refund address |
| Anyone, **before** the close round | Encrypted bid blobs | Bid amounts, refund addresses. No one can decrypt, including the seller, operator and arbiter |
| Anyone, **after** the close round | Every bid, the clearing price, every refund | Bidder identities |
| Vault signers (2-of-3 FROST) | Same as an auditor, plus the right to co-sign settlement | Any single signer's ability to move funds alone |
| Coordinator (relay + registry) | Public auction metadata, FROST round messages (commitments and signature shares) | Keys, bids before close, bidder identities |
| Lightwalletd / Zaino server | Your IP address and the timing of your sync and broadcast | Note contents (trial decryption runs on your device) |

## Guarantees

1. **Sealed until the bell.** A bid is encrypted with drand timelock encryption (tlock, quicknet) to the close round. Nobody has the key until the drand network publishes that round's signature.
2. **Shielded end to end.** Every bond, settlement, refund and escrow transaction is a v6 transaction with only an Ironwood bundle: no transparent inputs or outputs, no Sapling, no Orchard. A test enforces this (`ironwood_only` check).
3. **Unlinkable refunds.** Each bid carries a fresh diversified Ironwood address. Diversified addresses from the same wallet cannot be linked without its viewing key.
4. **No amount fingerprinting.** Bonds use the ZIP 318 canonical denominations (1, 2 or 5 times a power of ten), the same amounts every wallet uses while migrating out of Orchard, so a bond looks like ordinary network traffic. The real bid lives only inside the sealed memo.
5. **Audit without trust.** `tacit verify <auction>` rebuilds the bid set, the clearing price and the expected settlement from chain data plus the drand beacon, and checks them against the settlement transaction.
6. **No unilateral custody.** Vault funds need 2 of 3 signers (FROST, ZIP 312). The seller alone can't take bonds, and the operator alone can't either.

## Known limits (stated plainly)

- **Collusion:** any 2 of the 3 vault signers can move vault funds. Pick an arbiter you trust not to collude with the other side.
- **Cross-chain leg:** NEAR Intents supports only transparent Zcash addresses. When you fund from BTC, ETH, SOL or USDC, the foreign-chain transaction and one **fresh, single-use** Zcash t-address are public. Tacit shields that t-address right away. The observer learns "someone swapped X into ZEC", never which auction or bid.
- **Timing:** a bond sent seconds after an auction is announced, from a wallet whose activity you can watch at the network level, can be correlated. Mitigations: shield ahead of time, and optionally delay broadcasting.
- **Network layer:** your light-wallet server sees your IP. Use Tacit's own Zaino endpoint, run your own, or connect through Tor.
- **drand liveness:** if the drand network stops, bids stay sealed until it resumes. Funds stay safe in the vault; signers can refund bonds without decrypting.
- **Quantum recoverability:** vault keys are FROST group keys, and today the only way to build a viewing key around one uses the non quantum recoverable derivation (ZIP 2005). Funds in a vault are as safe as any Ironwood note against classical attackers, but they wouldn't be covered by a future quantum recovery path. Personal wallet funds use the standard derivation and are covered.
