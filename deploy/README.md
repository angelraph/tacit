# Deploying a Tacit node

One container runs the coordinator (which also serves the web app and the
chain gateway) and an operator and an arbiter agent on Zcash testnet.

Build the pieces on Linux (or WSL):

```bash
cargo build --release -p tacit-coordinator -p tacit-cli
TACIT_WASM_TARGET=$HOME/tacit-web-target sh web/build.sh
sh deploy/assemble.sh            # copies everything into deploy/out
```

Then deploy `deploy/out` with its Dockerfile (Railway, Fly, any Docker host).
Mount a volume at `/data`: it holds the coordinator database and the agents'
vault key shares, which must survive restarts.

| Variable | Default |
|---|---|
| `PORT` | `8080` |
| `TACIT_LIGHTWALLETD` | `https://testnet.zec.rocks:443` |
| `TACIT_NETWORK` | `test` |
| `DATA_DIR` | `/data` |
