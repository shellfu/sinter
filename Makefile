# sinter — developer entry points.
#
# Every target is a thin wrapper over cargo; the Makefile adds no logic of
# its own (orchestration lives in the binary, R6 — and in CI, which runs
# exactly `make gate` plus the nightly `make test-scale`).
#
# `make` or `make help` lists targets. Variables you may override:
#
#   REPO=/path/to/repo   target repository for bench/doctor (default: .)
#   PROFILE=release      cargo profile for `make build` (default: dev)

.DEFAULT_GOAL := help
REPO ?= .

# ---------------------------------------------------------------- building

.PHONY: build
build: ## Compile the workspace (dev profile; PROFILE=release for optimized)
ifeq ($(PROFILE),release)
	cargo build --release
else
	cargo build
endif

.PHONY: release
release: ## Compile the optimized sinter binary (target/release/sinter)
	cargo build --release

# ----------------------------------------------------------------- testing

.PHONY: test
test: ## Run the full test suite (unit, property, integration, goldens)
	cargo test --workspace

.PHONY: test-golden
test-golden: ## Golden corpus only: extraction + resolution accuracy (P/R 1.0 gate)
	cargo test -p sinter-extract --test golden -- --nocapture
	cargo test -p sinter-resolve --test golden_resolution -- --nocapture

.PHONY: test-scale
test-scale: ## 500k-node scale exercise + incremental gate, release mode (nightly CI job)
	cargo test --release -p sinter-store --test scale -- --ignored --nocapture
	cargo test --release -p sinter-cli --test incremental_build -- --nocapture

# ------------------------------------------------------------------ hygiene

.PHONY: fmt
fmt: ## Format all code in place
	cargo fmt

.PHONY: fmt-check
fmt-check: ## Fail if any file is unformatted (CI form of fmt)
	cargo fmt --check

.PHONY: lint
lint: ## Clippy across all targets; warnings are errors
	cargo clippy --all-targets -- -D warnings

.PHONY: gate
gate: fmt-check lint test ## Everything the PR CI gate runs, in CI order
	@echo "gate: all green"

# ------------------------------------------------------------ installation

.PHONY: install
install: ## Install the sinter binary (cargo install) and the Claude skill card
	cargo install --path crates/sinter-cli
	sinter install

.PHONY: doctor
doctor: release ## Diagnose the installation and REPO's graph (REPO=. by default)
	./target/release/sinter doctor $(REPO)

# ------------------------------------------------------------- measurement

.PHONY: bench
bench: release ## Manual perf check on REPO: full build, then no-op rebuild timing
	@echo "== full/incremental build =="
	./target/release/sinter build $(REPO)
	@echo "== no-op rebuild (should be fast and change nothing) =="
	./target/release/sinter build $(REPO)

# -------------------------------------------------------------------- misc

.PHONY: clean
clean: ## Remove build artifacts (cargo clean); graphs in repos are untouched
	cargo clean

.PHONY: help
help: ## List targets with their descriptions
	@awk 'BEGIN {FS = ":.*## "; printf "\nsinter make targets:\n\n"} \
		/^[a-zA-Z_-]+:.*## / {printf "  \033[1m%-13s\033[0m %s\n", $$1, $$2} \
		END {print ""}' $(MAKEFILE_LIST)
