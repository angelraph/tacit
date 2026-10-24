# Tacit Threat Model

> The price is public. You are not.

This document says exactly what Tacit hides, from whom, and when, and where it does **not** protect you.
If any claim here doesn't hold in the code, that is a bug. Please report it.

## Actors

| Actor | Can see | Cannot see |
|---|---|---|
| Public chain observer | That Ironwood transactions happened, plus their fees | Senders, recipients, amounts, memos |
| Auction auditor (holds the published vault IVK/OVK) | Every bond amount and every sealed-bid memo reaching the vault; every settlement output | Who sent any bond; which wallet owns any refund address |
| Anyone, **before** the close round | How many bids have arrived, and encrypted bid blobs | Bid amounts, refund addresses, bidders. No one can decrypt, including the seller, operator and arbiter |
| Anyone, **after** the close round | Every bid, the clearing price, every refund | Bidder identities |
| Vault signers (2-of-3 FROST) | Same as an auditor, plus the right to co-sign settlement | Any single signer's ability to move funds alone |
| Coordinator (relay + registry) | Public auction metadata, FROST round messages (commitments and signature shares) | Keys, bids before close, bidder identities |
| Lightwalletd / Zaino server | Your IP address and the timing of your sync and broadcast | Note contents (trial decryption runs on your device) |

## Guarantees

1. **Sealed until the bell.** A bid is encrypted with drand timelock encryption (tlock, quicknet) to the close round. Nobody has the key until the drand network publishes that round's signature.
2. **Shielded end to end.** Every bond, settlement, refund and escrow transaction is a v6 transaction with only an Ironwood bundle: no transparent inputs or outputs, no Sapling, no Orchard. A test enforces this (`ironwood_only` check).
3. **Unlinkable refunds.** Each bid carries a fresh diversified Ironwood address. Diversified addresses from the same wallet cannot be linked without its viewing key.
4. **No amount fingerprinting.** Every bid in an auction deposits exactly the same bond, fixed in the terms. Deposits are identical to each other, so watching them arrive tells you nothing about any bid. The bond is also a ZIP 318 canonical denomination (1, 2 or 5 times a power of ten), the same amounts every wallet uses while migrating out of Orchard. The real bid lives only inside the sealed memo.
5. **Audit without trust.** `tacit verify <auction>` rebuilds the bid set, the clearing price and the expected settlement from chain data plus the drand beacon, and checks them against the settlement transaction.
6. **No unilateral custody.** Vault funds need 2 of 3 signers (FROST, ZIP 312). The seller alone can't take bonds, and the operator alone can't either.

## Known limits (stated plainly)

- **Bid count:** the vault's viewing key is public, so the number of bids is visible while the auction is open. Their sizes and senders are not.

- **Collusion:** any 2 of the 3 vault signers can move vault funds. Pick an arbiter you trust not to collude with the other side.
- **Cross-chain leg:** when you fund from BTC, ETH, SOL or USDC through NEAR Intents 1Click, the foreign-chain deposit is public on that chain, and so is the fact that someone swapped into ZEC. The ZEC itself is delivered to a fresh shielded address of your wallet, so nothing on the Zcash side links it to an auction, a bid or an escrow.
- **Timing:** a bond sent seconds after an auction is announced, from a wallet whose activity you can watch at the network level, can be correlated. Mitigations: shield ahead of time, and optionally delay broadcasting.
- **Network layer:** your light-wallet server sees your IP. Use Tacit's own Zaino endpoint, run your own, or connect through Tor.
- **drand liveness:** if the drand network stops, bids stay sealed until it resumes. Funds stay safe in the vault; signers can refund bonds without decrypting.
- **Quantum recoverability:** vault keys are FROST group keys, and today the only way to build a viewing key around one uses the non quantum recoverable derivation (ZIP 2005). Funds in a vault are as safe as any Ironwood note against classical attackers, but they wouldn't be covered by a future quantum recovery path. Personal wallet funds use the standard derivation and are covered.

## The web app, agents and escrow

- **Keys stay in the tab.** The browser wallet derives its keys from a BIP 39 phrase inside a Web Worker running WebAssembly. Syncing downloads compact blocks for a whole height range and trial-decrypts them locally, so the chain gateway learns nothing about which notes are yours. Only finished, fully shielded transactions leave the browser.
- **The gateway sees what a light wallet server sees:** your IP, when you sync and when you broadcast. It never receives an address, a viewing key or a note. Run your own coordinator, or reach it over Tor, if your IP matters.
- **Browser storage.** The recovery phrase, the seat key and vault key shares are stored encrypted with AES-GCM under a key derived from your passcode (PBKDF2-SHA256, 310,000 rounds). Someone with your device and your passcode can spend; write the phrase down and don't reuse the passcode.
- **Agents are cosigners, not custodians.** An operator or arbiter agent holds one share of each vault it sits in. It signs an auction settlement only if it pays exactly what the rules compute, and an escrow payout only if a party is giving up its own claim (buyer pays seller, seller refunds buyer). It never signs a payment to its own advantage. Two colluding seats can still move funds, as with any 2-of-3 vault; choose agents run by people you trust not to collude with your counterparty.
- **Escrow privacy.** The escrow's terms, viewing key included, are published so the outcome can be audited. On chain, observers see only Ironwood transactions. Anyone holding the terms sees the amount and where it went, but not who the buyer and seller are, beyond their payout addresses.
- **Cross-chain funding.** The web app asks 1Click to deliver ZEC to a fresh shielded address of your wallet. The origin-chain deposit is public on that chain, as is the fact that someone swapped into ZEC.
