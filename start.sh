#!/bin/sh
# Runs a complete Tacit node in one container: the coordinator (registry,
# seat mailboxes, chain gateway and the web app) plus an operator agent and
# an arbiter agent, all on Zcash testnet through a public lightwalletd.
set -e
DATA="${DATA_DIR:-/data}"
NET="${TACIT_NETWORK:-test}"
export TACIT_LIGHTWALLETD="${TACIT_LIGHTWALLETD:-https://testnet.zec.rocks:443}"
export TACIT_WEB=/app/web
export TACIT_DB="$DATA/coordinator.sqlite"
export PORT="${PORT:-8080}"
mkdir -p "$DATA"

/app/bin/tacit-coordinator &
COORD="http://127.0.0.1:$PORT"
# Wait for the coordinator before the agents announce themselves.
i=0
until wget -q -O /dev/null "$COORD/v1/agents" || [ $i -ge 60 ]; do i=$((i + 1)); sleep 1; done

for role in operator arbiter; do
  home="$DATA/$role"
  if [ ! -f "$home/config.json" ]; then
    /app/bin/tacit --network "$NET" --home "$home" init --server "$TACIT_LIGHTWALLETD" --coordinator "$COORD"
  fi
  name="Tacit $role"
  /app/bin/tacit --network "$NET" --home "$home" agent --role "$role" --name "$name" &
done

# If any process dies, exit so the platform restarts the container.
wait -n 2>/dev/null || wait
exit 1
