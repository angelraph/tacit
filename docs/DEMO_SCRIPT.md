# Tacit demo video script

Target length: about 3 minutes. Record at 1440p in Chrome, with the app installed as a window (press "Install" in the header first) so the browser chrome stays out of the shot. Use testnet throughout.

Live app: https://tacit-production-6a96.up.railway.app
Code: https://github.com/angelraph/tacit

---

## 0:00 to 0:15. The problem

**Screen:** the landing page, ticker moving, the ring animating.

**Say:**
"Every auction on a public chain leaks. Bids are visible, so people snipe them. Bidders are visible, so whales get front-run and buyers get tracked. Tacit fixes that on Zcash: the price is public, the participants aren't."

## 0:15 to 0:35. What it is

**Screen:** scroll the landing page slowly through "How it works" and the stats.

**Say:**
"Tacit runs sealed-bid auctions and shielded escrow on Zcash's Ironwood pool. Bids are encrypted to a future drand round, so nobody, not even us, can open them early. Money sits in 2-of-3 FROST vaults that no single party controls."

## 0:35 to 1:00. A wallet in ten seconds

**Screen:** open Wallet. Create a wallet, choose 12 words, show the phrase, set a passcode. Then press "Get free TAZ".

**Say:**
"The wallet runs entirely in this tab: keys, syncing and zero-knowledge proofs. Pick a 12 or 24 word phrase. On testnet, one tap gets you coins from Tacit's own faucet, which already works on the NU7 upgrade."

**Show:** the QR code on the receive panel, then the balance arriving after a block.

## 1:00 to 1:30. Sell something

**Screen:** Sell. Fill in an item, a reserve, a bond, a short bidding window, second-price rule. Publish.

**Say:**
"A seller lists an item. Their browser, an operator agent and an arbiter agent run distributed key generation and create a fresh vault. The whole key never exists anywhere."

**Show:** the progress steps finishing and the auction appearing in the market table.

## 1:30 to 2:05. Bid three ways

**Screen:** open the auction. Show the three tabs.

**Say:**
"Bidders can use the Tacit wallet, a browser extension like Noir Wallet, or any mobile wallet through a ZIP 321 payment request with a QR code. Every bid deposits the same bond, so the chain shows nothing about how much anyone bid."

**Show:** place a bid from the Tacit wallet (the proof runs in a few seconds), then switch to "Any wallet" and show the QR. If Noir is installed, show the one-click "Seal and pay with Noir Wallet".

## 2:05 to 2:35. Unseal and settle

**Screen:** the auction after the close round. Countdown hits zero, bids unseal.

**Say:**
"When drand publishes the round, everyone can open every bid at the same moment. The agents recompute the settlement from the published rules and cosign it. The winner pays the second price, and every bond comes back to a fresh shielded address."

## 2:35 to 2:50. Verify it yourself

**Screen:** press "Run audit in this tab".

**Say:**
"You don't have to trust us. The audit finds every deposit with the vault's viewing key, verifies the drand beacon, opens the bids, and checks that the vault paid exactly what the rules require."

## 2:50 to 3:00. Close

**Screen:** Escrow page briefly, then the landing hero.

**Say:**
"The same vaults power private escrow between strangers. Tacit: sealed markets on Zcash. Open source, live on testnet today."

---

## Before recording

- Fund the bidder wallet first. Faucet drips arrive after the next block, about 25 seconds.
- Use a bidding window of 10 to 15 minutes so the unseal happens on camera without long waits.
- Check that both agents show online on the Agents page.
- Close other tabs; proving uses every core.
