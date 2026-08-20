# sinter — developer entry points.
#
# Every target is a thin wrapper over cargo; the Makefile adds no logic of
# its own (orchestration lives in the binary, R6 — and in CI, which runs
# the `make gate` checks plus the nightly `make test-scale`).
#
# `make` or `make help` lists targets. Variables you may override:
#
#   REPO=/path/to/repo   target repository for bench/doctor (default: .)
#   PROFILE=release      cargo profile for `make build` (default: dev)
#
# Versioning: the single source of truth is [workspace.package] version in
# Cargo.toml; every crate inherits it and the binary reports it via
# CARGO_PKG_VERSION (`sinter version`, `sinter --version`, doctor). Git tags
# follow it, never lead it: `make bump VERSION=x.y.z` writes Cargo.toml,
# commits, and tags vx.y.z in one step (nothing is pushed).

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
	cargo test --release -p sinter-resolve --test scale -- --ignored --nocapture
	cargo test --release -p sinter-io --test incremental_build -- --nocapture

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

.PHONY: audit
audit: ## Fail on known-vulnerable dependencies (requires cargo-audit)
	@command -v cargo-audit >/dev/null 2>&1 \
		|| { echo "error: cargo-audit is required; run: cargo install cargo-audit --locked"; exit 1; }
	cargo audit

.PHONY: gate
gate: fmt-check lint test audit ## Everything the blocking CI gate runs
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

.PHONY: version
version: ## Print the workspace version (source of truth: Cargo.toml [workspace.package])
	@cargo metadata --no-deps --format-version 1 \
		| grep -o '"name":"sinter-io","version":"[^"]*"' \
		| head -1 | sed 's/.*"version":"\([^"]*\)"/\1/'

.PHONY: bump
bump: ## Set version + commit + tag: make bump VERSION=x.y.z (local only, push yourself)
ifndef VERSION
	$(error usage: make bump VERSION=x.y.z)
endif
	@echo "$(VERSION)" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$$' \
		|| { echo "error: VERSION must be x.y.z, got '$(VERSION)'"; exit 1; }
	@git diff --quiet && git diff --cached --quiet \
		|| { echo "error: working tree not clean — commit or stash first"; exit 1; }
	@git rev-parse -q --verify "refs/tags/v$(VERSION)" >/dev/null \
		&& { echo "error: tag v$(VERSION) already exists"; exit 1; } || true
	$(MAKE) gate
	sed -i 's/^version = ".*"/version = "$(VERSION)"/' Cargo.toml
	sed -i -E 's/^(sinter-[a-z]+ = \{ version = )"[^"]+"/\1"$(VERSION)"/' Cargo.toml
	cargo update --workspace --quiet   # sync Cargo.lock to the new version
	git add Cargo.toml Cargo.lock
	git commit -m "chore: release $(VERSION)"
	git tag "v$(VERSION)"
	@echo "tagged v$(VERSION) — push with: git push origin main v$(VERSION)"

.PHONY: clean
clean: ## Remove build artifacts (cargo clean); graphs in repos are untouched
	cargo clean

.PHONY: help
help: ## List targets with their descriptions
	@awk 'BEGIN {FS = ":.*## "; printf "\nsinter make targets:\n\n"} \
		/^[a-zA-Z_-]+:.*## / {printf "  \033[1m%-13s\033[0m %s\n", $$1, $$2} \
		END {print ""}' $(MAKEFILE_LIST)
