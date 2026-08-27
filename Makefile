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
FRONTDOOR_IMAGE := $(REGISTRY)/beenet-frontdoor:$(VERSION)

# Build context is the beenet/ workspace root.
# spin/ is NOT needed: beenet-worker/beenet-factors (the spin users) are stripped
# from the workspace inside the Dockerfile before cargo resolves dependencies.
DOCKER_CTX := .

# ── Rust / Dev ───────────────────────────────────────────────────────────────
.PHONY: build
build: ## Build release binaries (registry + gateway + frontdoor)
	cargo build --release -p beenet-registry -p beenet-gateway -p beenet-frontdoor

.PHONY: build-debug
build-debug: ## Build debug binaries (registry + gateway)
	cargo build -p beenet-registry -p beenet-gateway -p beenet-frontdoor

.PHONY: test
test: ## Run all workspace tests
	cargo test --workspace

GUEST_VM_CACHE ?= $(HOME)/Library/Caches/beenet/vm/alpine-3.24.1
GUEST_KERNEL := $(GUEST_VM_CACHE)/extracted/boot/Image
GUEST_INITRD := $(GUEST_VM_CACHE)/beenet-alpine-3.24.1-aarch64-initramfs.img

.PHONY: guest-image
guest-image: ## Build the Alpine kernel + musl guest worker initramfs
	chmod +x scripts/build-macos-vm-image.sh
	scripts/build-macos-vm-image.sh

.PHONY: app-macos
app-macos: guest-image ## Build the unsigned macOS contributor app
	cargo build --release -p beenet-worker
	chmod +x apps/macos-contributor/build.sh
	apps/macos-contributor/build.sh

.PHONY: dmg
dmg: app-macos ## Wrap the macOS contributor app into a DMG
	chmod +x scripts/package-macos-dmg.sh
	scripts/package-macos-dmg.sh

LINUX_ARCH ?= $(shell uname -m)
LINUX_TARBALL := out/linux/beenet-worker-linux-$(LINUX_ARCH).tar.gz

.PHONY: linux-worker
linux-worker: ## Build the Linux beenet-worker binary
	cargo build --release -p beenet-worker
	chmod +x scripts/install-linux-worker.sh scripts/get-bworker.sh

.PHONY: linux-worker-tarball
linux-worker-tarball: linux-worker ## Package beenet-worker + install script into a tarball
	mkdir -p out/linux/beenet-worker-linux-$(LINUX_ARCH)
	cp target/release/beenet-worker out/linux/beenet-worker-linux-$(LINUX_ARCH)/beenet-worker
	strip out/linux/beenet-worker-linux-$(LINUX_ARCH)/beenet-worker
	cp scripts/install-linux-worker.sh out/linux/beenet-worker-linux-$(LINUX_ARCH)/install-linux-worker.sh
	cp deploy/linux/beenet-worker.service out/linux/beenet-worker-linux-$(LINUX_ARCH)/beenet-worker.service
	chmod +x out/linux/beenet-worker-linux-$(LINUX_ARCH)/beenet-worker \
		out/linux/beenet-worker-linux-$(LINUX_ARCH)/install-linux-worker.sh
	tar -C out/linux -czf $(LINUX_TARBALL) beenet-worker-linux-$(LINUX_ARCH)
	rm -rf out/linux/beenet-worker-linux-$(LINUX_ARCH)
	@ls -lh $(LINUX_TARBALL)

.PHONY: fmt
fmt: ## Format all code with rustfmt
	cargo fmt --all

.PHONY: fmt-check
fmt-check: ## Check formatting (non-destructive)
	cargo fmt --all -- --check

.PHONY: lint
lint: ## Run clippy on registry + gateway
	cargo clippy -p beenet-registry -p beenet-gateway -p beenet-frontdoor -- -D warnings

.PHONY: check
check: fmt-check lint ## Run all static checks (fmt + clippy)

# ── Docker ───────────────────────────────────────────────────────────────────
.PHONY: docker-build
docker-build: docker-build-registry docker-build-gateway docker-build-frontdoor ## Build all Docker images

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

.PHONY: docker-build-frontdoor
docker-build-frontdoor: ## Build beenet-frontdoor Docker image
	docker build -f docker/Dockerfile.frontdoor -t $(FRONTDOOR_IMAGE) $(DOCKER_CTX)

.PHONY: docker-push
docker-push: ## Push images to the registry
	docker push $(REGISTRY_IMAGE)
	docker push $(GATEWAY_IMAGE)
	docker push $(FRONTDOOR_IMAGE)

.PHONY: docker-tag-latest
docker-tag-latest: ## Re-tag current VERSION as :latest
	docker tag $(REGISTRY_IMAGE) $(REGISTRY)/beenet-registry:latest
	docker tag $(GATEWAY_IMAGE)  $(REGISTRY)/beenet-gateway:latest
	docker tag $(FRONTDOOR_IMAGE) $(REGISTRY)/beenet-frontdoor:latest

.PHONY: docker-release
docker-release: docker-build docker-push docker-tag-latest ## Build, push, and tag as latest

.PHONY: docker-up
docker-up: ## Phased local stack via scripts/dev-up.sh (Docker services + host worker tokens)
	./scripts/dev-up.sh up --build

.PHONY: docker-down
docker-down: ## Stop docker compose dev stack
	./scripts/dev-up.sh down

# ── Kubernetes ───────────────────────────────────────────────────────────────
.PHONY: ensure-routing-secret
ensure-routing-secret: ## Create routing tokens once; preserve them on subsequent deploys
	./scripts/ensure-routing-secret.sh

.PHONY: deploy
deploy: ensure-routing-secret ## Apply Kubernetes manifests (registry + gateway + frontdoor)
	kubectl apply -f beenet-deploy/registry.yaml
	kubectl apply -f beenet-deploy/gateway.yaml
	kubectl apply -f beenet-deploy/frontdoor.yaml
	kubectl apply -f beenet-deploy/ingress.yaml

.PHONY: deploy-registry
deploy-registry: ensure-routing-secret ## Deploy only beenet-registry
	kubectl apply -f beenet-deploy/redis.yaml
	kubectl apply -f beenet-deploy/registry.yaml

.PHONY: deploy-gateway
deploy-gateway: ensure-routing-secret ## Deploy only beenet-gateway
	kubectl apply -f beenet-deploy/gateway.yaml

.PHONY: deploy-frontdoor
deploy-frontdoor: ensure-routing-secret ## Deploy only beenet-frontdoor
	kubectl apply -f beenet-deploy/frontdoor.yaml

.PHONY: undeploy
undeploy: ## Remove all beenet Kubernetes resources
	kubectl delete -f beenet-deploy/gateway.yaml  --ignore-not-found
	kubectl delete -f beenet-deploy/frontdoor.yaml --ignore-not-found
	kubectl delete -f beenet-deploy/ingress.yaml --ignore-not-found
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
