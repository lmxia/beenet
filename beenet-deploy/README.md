# beenet-deploy

Helm charts for deploying beenet on Kubernetes.

## Charts

| Chart | Description |
|-------|-------------|
| `charts/beenet-registry` | Worker registry with Redis persistence + admin API |
| `charts/beenet-gateway` | HTTP gateway that dispatches invocations to registered workers |

## Quick start

```bash
# Install registry (Redis single-pod is included in the same chart)
helm install beenet-registry ./charts/beenet-registry

# Create gateway identity secret once (do not commit the key):
#   kubectl -n beenet create secret generic beenet-gateway-identity \
#     --from-file=identity.key=./identity.key

# Install gateway (workers dial its public libp2p LoadBalancer address)
helm install beenet-gateway ./charts/beenet-gateway \
  --set registryUrl=http://beenet-registry-beenet-registry:3030

# Or plain manifests:
#   kubectl apply -f registry.yaml
#   kubectl apply -f gateway.yaml
```

## beenet-registry

### Architecture

```
┌──────────────────────┐      HSET/HDEL/HGETALL      ┌────────────┐
│   beenet-registry    │◄──────────────────────────► │   Redis    │
│   (stateless pod)    │                              │            │
└──────────────────────┘                              └────────────┘
         ▲  POST /v1/workers/join (signed)
         │  POST /v1/workers/heartbeat (signed)
    workers (beenet-worker)
```

- **Stateless pod**: all registration state lives in Redis; the pod can restart or scale freely.
- **Admin token**: generated fresh each pod start, printed to stdout (`kubectl logs`).
- **Join tokens**: created by admin via `POST /v1/admin/tokens`; workers use them once.
- **Signed heartbeats**: workers sign every heartbeat with their Ed25519 private key; no token required after initial registration.

### Key values

```yaml
# Redis is a plain single-pod Deployment + Service in the same chart.
# Image and resources:
redis:
  image: redis:7-alpine
  resources:
    limits:
      memory: 128Mi

# Disable PVC (use emptyDir, data lost on pod restart):
redis:
  persistence:
    enabled: false

# Expose registry externally (e.g. for CLI admin access):
service:
  type: LoadBalancer
```

### Get the admin token after deploy

```bash
kubectl logs deployment/beenet-registry-beenet-registry | grep -A1 "ADMIN TOKEN"
```

## beenet-gateway

### Key values

```yaml
registryUrl: "http://beenet-registry-beenet-registry:3030"
registryPollMs: 2000
defaultDeadlineMs: 10000

# Expose gateway publicly
service:
  type: LoadBalancer
  port: 8080
```
