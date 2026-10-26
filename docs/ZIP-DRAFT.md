# Draft ZIP: Sealed-Bid Auctions and Escrow with Threshold Vaults on Ironwood

```
ZIP: unassigned
Title: Sealed-Bid Auctions and Escrow with Threshold Vaults on Ironwood
Owners: Tacit contributors <https://github.com/angelraph/tacit>
Status: Draft
Category: Standards / Wallet
Created: 2026-10-26
License: MIT
Discussions-To: https://github.com/angelraph/tacit/issues
```

## Terminology

The key words MUST, MUST NOT, SHOULD and MAY are to be interpreted as in BCP 14 (RFC 2119, RFC 8174) when, and only when, they appear in all capitals.

- **Vault**: an Ironwood shielded address whose spend authority is shared 2 of 3 with FROST (ZIP 312), and whose full viewing key is public.
- **Seat**: one of the three FROST participants of a vault, identified by an X25519 public key.
- **Terms**: the public, immutable description of one auction or one escrow.
- **Bond**: the fixed amount every bid deposits into an auction's vault.
- **Beacon**: a drand quicknet round signature (chain hash `52db9ba70e0cc0f6eaf7803dd07447a1f5477735fd3f661792ba94600c84e971`).
- **Agent**: a seat that runs unattended and signs only what these rules compute.

## Abstract

This ZIP specifies how to run sealed-bid auctions and two-party escrow on Zcash without a trusted custodian and without smart contracts. Funds sit in a fresh 2-of-3 FROST vault per market. Bids are encrypted to a future drand round with timelock encryption and carried in shielded memos, so nobody, including the seller and the vault's signers, can read a bid before the round is published. Every bid deposits an identical bond, so the chain reveals nothing about bid sizes. The vault's viewing key is part of the public terms, so anyone can recompute the outcome from chain data and the beacon and check that the vault paid exactly what the rules require, without learning who took part.

## Motivation

Auctions on transparent chains leak every bid and every bidder, which invites sniping, front-running and surveillance. Zcash hides senders, receivers and amounts, but it has no general-purpose contracts, so until now a private auction on Zcash needed an auctioneer who sees every bid and holds every coin: the very party a sealed auction exists to avoid.

This ZIP removes that party with parts that already exist in the Zcash ecosystem (Ironwood, FROST, ZIP 302 memos, ZIP 317 fees) plus one public randomness beacon. It gives wallets, marketplaces and agents a common format, so a bid placed from any conforming wallet can be settled by any conforming signer and audited by anyone.

## Requirements

1. No party can read any bid before the closing round is published.
2. No single party can move a vault's funds.
3. A bid deposit reveals nothing about the bid amount to an observer holding the viewing key.
4. Anyone holding only the terms can verify a settlement exactly, offline after fetching chain data and the beacon.
5. A bid cannot be replayed into a different auction.
6. Every transaction is fully shielded in the Ironwood pool.

## Specification

### Vaults

A vault MUST be created by distributed key generation among its three seats, so that no seat ever holds the full spending key. Spending MUST require signature shares from two seats (FROST with rerandomized RedPallas, as in ZIP 312). The vault's unified full viewing key MUST be published in the terms. Vault transactions MUST be v6 transactions whose only shielded component is an Ironwood bundle, with no transparent inputs or outputs.

Every vault spend MUST include exactly one zero-valued change output back to the vault, so that a transaction spending a vault's whole balance has the same shape as an ordinary transaction.

### Auction terms and identifier

| Field | Type | Rule |
|---|---|---|
| `network` | string | `main`, `test` or `regtest` |
| `vault_ufvk` | string | the vault's unified full viewing key |
| `seller` | string | unified address that receives the price |
| `item` | string | at most 256 bytes |
| `created_height` | u32 | height the vault was created at |
| `reserve` | u64 | lowest acceptable price, zatoshi; MUST NOT exceed `bond` |
| `bond` | u64 | exact deposit per bid and highest possible bid; MUST be a ZIP 318 canonical denomination (1, 2 or 5 times a power of ten) |
| `bids_until_height` | u32 | deposits mined above this height are late |
| `close_round` | u64 | drand quicknet round the bids are sealed to |
| `rule` | u8 | 1 first price, 2 second price |

The auction id is `BLAKE2b-256(personalization = "TacitAuction__v1", data)`, where `data` is the fields in the order above. Strings are encoded as their UTF-8 byte length (u32 little endian) followed by the bytes; integers are little endian; `rule` is one byte.

### Sealed bid memo

A memo is 512 bytes:

```
0xF5 || "TACIT" || version (1) || kind (1 = sealed bid) || auction id (32) || len (u16 BE) || sealed box || zero padding
```

`0xF5` is the ZIP 302 marker for arbitrary binary data. The sealed box is

```
close round (u64 BE) || tlock ciphertext (128) || ChaCha20-Poly1305 ciphertext
```

The tlock ciphertext encrypts a random 16-byte file key to `close_round` under the quicknet public key. The payload key is `HKDF-SHA256(salt = "tacit/seal/v1", ikm = file key, info = round as u64 BE)`. The nonce is zero, because every key encrypts exactly one message. The associated data is the round followed by the tlock ciphertext. The plaintext is

```
version (1) || auction id (32) || bid in zatoshi (u64 BE) || refund receiver (43)
```

The refund receiver is the raw receiver of a fresh diversified Ironwood address of the bidder. The auction id inside the box MUST equal the one outside it; a mismatch makes the deposit unreadable.

**Text form.** Some wallet APIs accept only UTF-8 memos. A conforming wallet MAY send the memo as `"TACIT1:" || base64url_nopad(binary memo without its zero padding)`. Readers MUST accept both forms.

### Bidding

A bidder MUST send exactly `bond` zatoshi to the vault's address in an Ironwood-only transaction carrying the sealed bid memo. Bidders SHOULD use a fresh diversified refund address for every bid.

### Settlement

When the beacon for `close_round` exists, it MUST be verified against the quicknet public key before use. Then, for each deposit the viewing key finds (identified by txid and action index, 36 bytes):

1. If the memo is not a sealed bid for this auction, or does not open, the deposit is **unreadable** and its value goes to the seller.
2. Otherwise the deposit is refunded in full if it was mined above `bids_until_height`, if its value is not exactly `bond`, if the bid exceeds `bond`, or if the bid is below `reserve`.
3. Every remaining deposit is a valid bid.

Valid bids are ranked by amount, highest first. Ties go to the lower `BLAKE2b-256(personalization = "TacitTiebreak_v1", beacon randomness || deposit id)`. The price is the winning bid under first price; under second price it is the larger of the runner-up's bid and `reserve`.

Payouts: the winner receives `bond - price`; every other valid bid and every refunded deposit receives its full value; the seller receives the price plus unreadable deposits, minus the fee. The fee is `5000 * max(2, deposits, payouts + 1)` zatoshi (ZIP 317, counting the zero-valued change output). If the seller's share cannot cover the fee, the seller receives nothing and the fee is taken evenly from the refunds, any remainder from the first.

The vault MUST pay all outputs in a single transaction. A seat MUST NOT sign a settlement whose outputs differ from this computation.

### Escrow

An escrow vault's seats are, in order, buyer, seller and arbiter. Its terms are `network`, `vault_ufvk`, `seats` (three hex X25519 keys), `buyer` (refund address), `seller` (payment address), `item`, `amount` and `created_height`. The escrow id is `BLAKE2b-256(personalization = "TacitEscrow___v1", data)` with the same encoding as auction terms.

The vault pays out exactly once: everything it holds, to `seller` or to `buyer`, minus `5000 * max(2, notes spent)` zatoshi, plus the zero-valued change output. An agent arbiter MUST cosign automatically only when a party gives up its own claim (the buyer releasing to the seller, or the seller refunding the buyer); every other request is a dispute for a human decision.

### Verification

A verifier needs only the terms, a light wallet server and the beacon. It MUST import `vault_ufvk` watch-only, scan from `created_height`, collect every deposit and memo, compute the outcome as above, locate the single transaction that spends from the vault, recover its outputs with the vault's outgoing viewing keys, and compare them as a multiset with the computed payouts. Any missing or extra output is a mismatch. An auction with no deposits and no spend is closed with no bids.

## Rationale

- **Identical bonds** make deposits indistinguishable, so watching the vault fill reveals only the number of bids. The bid itself lives in the sealed memo, which also caps it at the bond.
- **drand timelock** means sealing needs no trusted party and revealing needs no cooperation: nobody can open early, and no bidder can refuse to reveal.
- **A published viewing key** turns "trust the auctioneer" into "check the auctioneer". It reveals amounts and memos to everyone, but never senders, and refunds go to fresh diversified addresses that cannot be linked to each other.
- **2-of-3 FROST** removes unilateral custody while keeping a path to settle when one seat is offline.
- **One settlement transaction** keeps the audit to a single comparison and gives observers one ordinary-looking transaction.

## Security Considerations

- **Collusion.** Any two seats can move a vault's funds. Sellers and buyers SHOULD choose agents run by independent parties; implementations SHOULD show who runs each seat and warn when two seats belong to one party.
- **What becomes public.** After the round, every bid amount is readable by anyone with the terms. Bidder identities are not. Before the round, only the number of deposits is visible.
- **Network metadata.** Light wallet servers see IP addresses and timing. Bidders SHOULD fund wallets in advance and MAY broadcast over Tor.
- **Beacon liveness.** If drand halts, bids stay sealed; funds stay in the vault, and two seats can refund bonds without opening bids.
- **Quantum recoverability.** FROST vault viewing keys are built with the non-recoverable derivation (ZIP 2005); vault funds are not covered by a future quantum recovery path. Personal wallets are.

## Test Vectors

`test-vectors/*.json` in the reference implementation are real settled auctions frozen from Zcash mainnet and testnet: terms, every deposit with its memo, the beacon, and the payments the settlement transaction made on chain. A conforming implementation MUST reproduce each vector's winner, price, fee and on-chain payments exactly, with no network access. The reference test is `cargo test -p tacit-core --test conformance`.

## Reference Implementation

https://github.com/angelraph/tacit: `tacit-core` (terms, memos, sealing, settlement), `tacit-vault` (FROST DKG and signing), `tacit-light` (browser light wallet and auditor), `tacit-cli` (wallet, seats and agents), `tacit-coordinator` (relay, registry and chain gateway).

## Deployment

Live on Zcash mainnet (https://tacit-mainnet.up.railway.app) and testnet (https://tacit-production-6a96.up.railway.app). FROST spend authorization currently needs `FullViewingKey::from_sk_ak`, available in `zcash/orchard#475`; implementations SHOULD move to the upstream crate once that is merged. Nothing in the wire formats depends on the fork.

## References

ZIP 302 (memo field), ZIP 312 (FROST for spend authorization), ZIP 317 (fees), ZIP 318 (canonical denominations), ZIP 321 (payment request URIs), ZIP 2005 (quantum recoverability), drand quicknet and tlock (https://drand.love), RFC 2119, RFC 8174.
