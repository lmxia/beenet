SHELL := /bin/bash
.DEFAULT_GOAL := help

# ── Config ──────────────────────────────────────────────────────────────────
# Override with: make docker-build REGISTRY=your.registry.io VERSION=v0.2.0
REGISTRY ?= ghcr.io/beenet
VERSION  ?= $(shell git describe --tags --always --dirty 2>/dev/null || echo "dev")
HTTP_PROXY  ?= http://host.docker.internal:7890
HTTPS_PROXY ?= http://host.docker.internal:7890

REGISTRY_IMAGE := $(REGISTRY)/beenet-registry:$(VERSION)
GATEWAY_IMAGE  := $(REGISTRY)/beenet-gateway:$(VERSION)

# Build context is the beenet/ workspace root.
# spin/ is NOT needed: beenet-worker/beenet-factors (the spin users) are stripped
# from the workspace inside the Dockerfile before cargo resolves dependencies.
DOCKER_CTX := .

# ── Rust / Dev ───────────────────────────────────────────────────────────────
.PHONY: build
build: ## Build release binaries (registry + gateway)
	cargo build --release -p beenet-registry -p beenet-gateway

.PHONY: build-debug
build-debug: ## Build debug binaries (registry + gateway)
	cargo build -p beenet-registry -p beenet-gateway

.PHONY: test
test: ## Run all workspace tests
	cargo test --workspace

.PHONY: fmt
fmt: ## Format all code with rustfmt
	cargo fmt --all

.PHONY: fmt-check
fmt-check: ## Check formatting (non-destructive)
	cargo fmt --all -- --check

.PHONY: lint
lint: ## Run clippy on registry + gateway
	cargo clippy -p beenet-registry -p beenet-gateway -- -D warnings

.PHONY: check
check: fmt-check lint ## Run all static checks (fmt + clippy)

# ── Docker ───────────────────────────────────────────────────────────────────
.PHONY: docker-build
docker-build: docker-build-registry docker-build-gateway ## Build all Docker images

.PHONY: docker-build-registry
docker-build-registry: ## Build beenet-registry Docker image
	docker build \
		--build-arg HTTP_PROXY=$(HTTP_PROXY) \
		--build-arg HTTPS_PROXY=$(HTTPS_PROXY) \
		-f docker/Dockerfile.registry \
		-t $(REGISTRY_IMAGE) \
		$(DOCKER_CTX)

.PHONY: docker-build-gateway
docker-build-gateway: ## Build beenet-gateway Docker image
	docker build \
		--build-arg HTTP_PROXY=$(HTTP_PROXY) \
		--build-arg HTTPS_PROXY=$(HTTPS_PROXY) \
		-f docker/Dockerfile.gateway \
		-t $(GATEWAY_IMAGE) \
		$(DOCKER_CTX)

.PHONY: docker-push
docker-push: ## Push images to the registry
	docker push $(REGISTRY_IMAGE)
	docker push $(GATEWAY_IMAGE)

.PHONY: docker-tag-latest
docker-tag-latest: ## Re-tag current VERSION as :latest
	docker tag $(REGISTRY_IMAGE) $(REGISTRY)/beenet-registry:latest
	docker tag $(GATEWAY_IMAGE)  $(REGISTRY)/beenet-gateway:latest

.PHONY: docker-release
docker-release: docker-build docker-push docker-tag-latest ## Build, push, and tag as latest

.PHONY: docker-up
docker-up: ## Start Redis + registry + gateway via docker compose
	docker compose -f docker/docker-compose.dev.yml up -d --build

.PHONY: docker-down
docker-down: ## Stop docker compose dev stack
	docker compose -f docker/docker-compose.dev.yml down

# ── Kubernetes ───────────────────────────────────────────────────────────────
.PHONY: deploy
deploy: ## Apply Kubernetes manifests (registry + gateway)
	kubectl apply -f beenet-deploy/registry.yaml
	kubectl apply -f beenet-deploy/gateway.yaml

.PHONY: deploy-registry
deploy-registry: ## Deploy only beenet-registry
	kubectl apply -f beenet-deploy/registry.yaml

.PHONY: deploy-gateway
deploy-gateway: ## Deploy only beenet-gateway
	kubectl apply -f beenet-deploy/gateway.yaml

.PHONY: undeploy
undeploy: ## Remove all beenet Kubernetes resources
	kubectl delete -f beenet-deploy/gateway.yaml  --ignore-not-found
	kubectl delete -f beenet-deploy/registry.yaml --ignore-not-found

.PHONY: status
status: ## Show running pods in the beenet namespace
	kubectl get pods -n beenet

.PHONY: logs-registry
logs-registry: ## Tail beenet-registry logs
	kubectl logs -n beenet -l app=beenet-registry -f

.PHONY: logs-gateway
logs-gateway: ## Tail beenet-gateway logs
	kubectl logs -n beenet -l app=beenet-gateway -f

# ── Help ─────────────────────────────────────────────────────────────────────
.PHONY: help
help: ## Show this help
	@echo ""
	@echo "Usage: make [target] [VAR=value ...]"
	@echo ""
	@echo "Variables:"
	@echo "  REGISTRY  Image registry prefix  (default: $(REGISTRY))"
	@echo "  VERSION   Image tag              (default: git describe)"
	@echo ""
	@echo "Targets:"
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-22s\033[0m %s\n", $$1, $$2}' | sort
	@echo ""
