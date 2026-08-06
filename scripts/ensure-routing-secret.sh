#!/usr/bin/env bash
set -euo pipefail

NAMESPACE="${BEENET_NAMESPACE:-beenet}"
SECRET_NAME="${BEENET_ROUTING_SECRET_NAME:-beenet-routing-tokens}"

command -v kubectl >/dev/null 2>&1 || {
  echo "kubectl is required" >&2
  exit 1
}
command -v openssl >/dev/null 2>&1 || {
  echo "openssl is required to generate routing tokens" >&2
  exit 1
}

kubectl create namespace "$NAMESPACE" --dry-run=client -o yaml | kubectl apply -f - >/dev/null

if kubectl get secret "$SECRET_NAME" -n "$NAMESPACE" >/dev/null 2>&1; then
  echo "routing secret $NAMESPACE/$SECRET_NAME already exists; keeping existing tokens"
else
  INTERNAL_TOKEN="$(openssl rand -hex 32)"
  FRONTDOOR_TOKEN="$(openssl rand -hex 32)"

  kubectl create secret generic "$SECRET_NAME" \
    -n "$NAMESPACE" \
    --from-literal=internal-token="$INTERNAL_TOKEN" \
    --from-literal=frontdoor-token="$FRONTDOOR_TOKEN" \
    >/dev/null

  echo "created routing secret $NAMESPACE/$SECRET_NAME"
fi

if kubectl get secret beenet-redis-auth -n "$NAMESPACE" >/dev/null 2>&1; then
  echo "Redis secret $NAMESPACE/beenet-redis-auth already exists; keeping existing password"
else
  REDIS_PASSWORD="$(openssl rand -hex 32)"
  kubectl create secret generic beenet-redis-auth \
    -n "$NAMESPACE" \
    --from-literal=redis-password="$REDIS_PASSWORD" \
    >/dev/null
  echo "created Redis secret $NAMESPACE/beenet-redis-auth"
fi
