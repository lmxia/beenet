#!/usr/bin/env bash
# Local E2E: registry + gateway + 2 workers + 2 CIDs.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

CID1=bafkreiciwbnrp6o3qzfs7jfpzx6y422pkzxmhktnpzrk5ydwwjstvafwsq
CID2=bafkreictt2soyhv22jgeelsym7uecfqbr6wyme5xtsaekwabxm5ztvh2ge
ADMIN=beenet-local-admin

pkill -f 'target/release/beenet-' 2>/dev/null || true
sleep 1

rm -rf /tmp/beenet-w1 /tmp/beenet-w2 /tmp/beenet-gw-id
mkdir -p /tmp/beenet-w1/wasm_cache /tmp/beenet-w2/wasm_cache /tmp/beenet-gw-id
cp "wasm_cache/${CID1}.wasm" "/tmp/beenet-w1/wasm_cache/${CID1}.wasm"
cp "wasm_cache/${CID2}.wasm" "/tmp/beenet-w2/wasm_cache/${CID2}.wasm"

./target/release/beenet-registry \
  --http-addr 127.0.0.1:3030 \
  --redis-url redis://127.0.0.1:6379 \
  --admin-token "$ADMIN" \
  >/tmp/beenet-registry.log 2>&1 &
REG_PID=$!

cleanup() {
  kill "${W1_PID:-}" "${W2_PID:-}" "${GW_PID:-}" "$REG_PID" 2>/dev/null || true
}
trap cleanup EXIT

for i in $(seq 1 50); do curl -sf http://127.0.0.1:3030/health >/dev/null && break; sleep 0.1; done

GW_TOKEN=$(curl -sS -X POST http://127.0.0.1:3030/v1/admin/gateway-tokens \
  -H "Authorization: Bearer $ADMIN" \
  -H "Content-Type: application/json" \
  -d '{"description":"local-e2e-gateway","ttl_secs":600}' \
  | python3 -c 'import sys,json; print(json.load(sys.stdin)["token_value"])')
printf '%s\n' "$GW_TOKEN" >/tmp/beenet-gateway-join-token
chmod 600 /tmp/beenet-gateway-join-token

./target/release/beenet-gateway \
  --http-addr 127.0.0.1:8080 \
  --registry-url http://127.0.0.1:3030 \
  --registry-poll-ms 500 \
  --libp2p-listen-addr /ip4/127.0.0.1/tcp/4001 \
  --public-addr /ip4/127.0.0.1/tcp/4001 \
  --identity-key-path /tmp/beenet-gw-id/identity.key \
  --name local-e2e-gw \
  --join-token-file /tmp/beenet-gateway-join-token \
  >/tmp/beenet-gateway.log 2>&1 &
GW_PID=$!

for i in $(seq 1 50); do curl -sf http://127.0.0.1:8080/health >/dev/null && break; sleep 0.1; done

TOKEN=$(curl -sS -X POST http://127.0.0.1:3030/v1/admin/tokens \
  -H "Authorization: Bearer $ADMIN" \
  -H "Content-Type: application/json" \
  -d '{"description":"local-e2e","ttl_secs":600}' \
  | python3 -c 'import sys,json; print(json.load(sys.stdin)["token_value"])')
printf '%s\n' "$TOKEN" >/tmp/beenet-join-token
chmod 600 /tmp/beenet-join-token

cat >/tmp/worker1.toml <<EOF
[worker]
registry_url = "http://127.0.0.1:3030"
name = "alice"
EOF
cat >/tmp/worker2.toml <<EOF
[worker]
registry_url = "http://127.0.0.1:3030"
name = "bob"
EOF

(cd /tmp/beenet-w1 && "$ROOT/target/release/beenet-worker" --config /tmp/worker1.toml --join-token-file /tmp/beenet-join-token) >/tmp/worker1.log 2>&1 &
W1_PID=$!
(cd /tmp/beenet-w2 && "$ROOT/target/release/beenet-worker" --config /tmp/worker2.toml --join-token-file /tmp/beenet-join-token) >/tmp/worker2.log 2>&1 &
W2_PID=$!

ok=0
for i in $(seq 1 60); do
  STATUS=$(curl -sf -H "Authorization: Bearer $ADMIN" http://127.0.0.1:3030/v1/dashboard/status || echo '{}')
  GW=$(python3 -c 'import json,sys; d=json.loads(sys.argv[1]); print(d.get("gateway_count",0))' "$STATUS")
  WC=$(python3 -c 'import json,sys; d=json.loads(sys.argv[1]); print(d.get("worker_count",0))' "$STATUS")
  echo "t=$i gateways=$GW workers=$WC"
  if [[ "$GW" == "1" && "$WC" == "2" ]]; then ok=1; break; fi
  sleep 0.5
done
if [[ "$ok" != "1" ]]; then
  echo "FAIL: status not ready"
  curl -sS -H "Authorization: Bearer $ADMIN" http://127.0.0.1:3030/v1/dashboard/status | python3 -m json.tool || true
  tail -40 /tmp/beenet-gateway.log /tmp/beenet-registry.log /tmp/worker1.log /tmp/worker2.log || true
  exit 1
fi

curl -sS -H "Authorization: Bearer $ADMIN" http://127.0.0.1:3030/v1/dashboard/status | python3 -m json.tool

code1=$(curl -sS -o /tmp/out1.bin -w '%{http_code}' -X POST "http://127.0.0.1:8080/run/ipfs/${CID1}" \
  -H 'content-type: application/octet-stream' --data-binary 'hello-w1')
code2=$(curl -sS -o /tmp/out2.bin -w '%{http_code}' -X POST "http://127.0.0.1:8080/run/ipfs/${CID2}" \
  -H 'content-type: application/octet-stream' --data-binary 'hello-w2')
echo "invoke cid1 http=$code1"
echo "invoke cid2 http=$code2"

if [[ "$code1" != "200" || "$code2" != "200" ]]; then
  echo "FAIL: invoke"
  tail -50 /tmp/beenet-gateway.log /tmp/worker1.log /tmp/worker2.log || true
  exit 1
fi

echo "OK"
