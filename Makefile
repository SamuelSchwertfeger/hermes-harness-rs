# Hermes Harness developer entrypoints. Run from the repository root.
.DEFAULT_GOAL := help
SHELL := /bin/sh
CARGO ?= cargo

.PHONY: help build release fmt fmt-check lint test deny machete check ci clean

help: ## Show supported commands
	@awk 'BEGIN {FS = ":.*## "} /^[a-zA-Z_-]+:.*## / {printf "%-18s %s\n", $$1, $$2}' $(MAKEFILE_LIST)

build: ## Debug build
	$(CARGO) build --locked

release: ## Release build
	$(CARGO) build --release --locked

fmt: ## Format Rust files (writes changes)
	$(CARGO) fmt

fmt-check: ## Check formatting without changes
	$(CARGO) fmt --check

lint: ## Clippy over all targets, warnings denied
	$(CARGO) clippy --all-targets --locked -- -D warnings

test: ## Run all tests
	$(CARGO) test --locked

deny: ## Check licenses, advisories and bans
	$(CARGO) deny check

machete: ## Find unused dependencies
	$(CARGO) machete

check: fmt-check lint test ## Run all local quality gates

ci: check deny machete ## Mirror CI checks

clean: ## Remove build outputs
	$(CARGO) clean
