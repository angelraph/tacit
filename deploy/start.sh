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
# The gateway caches blocks so wallet restores don't crawl through the public
# lightwalletd. Testnet is heavy, so cache only its recent stretch.
if [ "$NET" != "main" ]; then
  export TACIT_CACHE_FROM="${TACIT_CACHE_FROM:-4450000}"
fi
mkdir -p "$DATA"

# The testnet faucet: a wallet that drips TAZ to anyone who asks from the app.
# It takes donations on a transparent address too and shields them itself.
if [ "$NET" != "main" ]; then
  fhome="$DATA/faucet"
  if [ ! -f "$fhome/config.json" ]; then
    /app/bin/tacit --network "$NET" --home "$fhome" init --server "$TACIT_LIGHTWALLETD" --coordinator "http://127.0.0.1:$PORT"
  fi
  /app/bin/tacit --network "$NET" --home "$fhome" faucet --listen 127.0.0.1:8091 --amount "${TACIT_DRIP:-0.05}" &
  export TACIT_FAUCET="http://127.0.0.1:8091"
fi

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
  /app/bin/tacit --network "$NET" --home "$home" agent --role "$role" --name "$name" --run-by Tacit &
done

# On testnet, keep a practice lot open at all times so anyone (a judge, a
# first-time visitor) can seal a bid with faucet coins and watch it settle.
# Every lot is a fresh vault shared with the two agents above. Nothing is
# shipped to the winner; the item says so.
if [ "$NET" != "main" ] && [ "${TACIT_PRACTICE_LOTS:-1}" = "1" ]; then
  (
    hh="$DATA/house"
    if [ ! -f "$hh/config.json" ]; then
      /app/bin/tacit --network "$NET" --home "$hh" init --server "$TACIT_LIGHTWALLETD" --coordinator "$COORD" > /dev/null 2>&1
    fi
    sleep 90
    op=$(/app/bin/tacit --network "$NET" --home "$DATA/operator" seat)
    ar=$(/app/bin/tacit --network "$NET" --home "$DATA/arbiter" seat)
    while true; do
      name="practice-$(date +%s)"
      if timeout 600 /app/bin/tacit --network "$NET" --home "$hh" vault new "$name" --seats "me,$op,$ar"; then
        timeout 300 /app/bin/tacit --network "$NET" --home "$hh" auction create --vault "$name" \
          --item "Practice lot $(date -u +%H:%M) UTC: try a sealed bid with free testnet coins (nothing is shipped)" \
          --reserve 0.0001 --bond 0.001 --bid-blocks "${TACIT_PRACTICE_BLOCKS:-48}" --close-in "${TACIT_PRACTICE_CLOSE:-1500}" || true
      fi
      sleep "${TACIT_PRACTICE_EVERY:-900}"
    done
  ) &
fi

# If any process dies, exit so the platform restarts the container.
wait -n 2>/dev/null || wait
exit 1
