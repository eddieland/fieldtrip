# The checks CI runs, one target each. `make` alone lists them.
#
# Every warning is an error, here as in CI. CI calls these same targets, one per job, so the commands live in one
# place. To document a new target, put `## what it does` after its name.

export RUSTFLAGS := -D warnings
export RUSTDOCFLAGS := -D warnings

.DEFAULT_GOAL := help

.PHONY: help
help: ## List the targets
	@awk 'BEGIN { FS = ":.*## " } /^[a-zA-Z_-]+:.*## / { printf "  %-8s %s\n", $$1, $$2 }' $(MAKEFILE_LIST)

.PHONY: check
check: fmt clippy doc test ## Run the full check: fmt, clippy, doc and test

.PHONY: fmt
fmt: ## Check the formatting, without changing a file
	cargo fmt --all --check

.PHONY: clippy
clippy: ## Lint every target with the strict set in Cargo.toml
	cargo clippy --workspace --all-targets --all-features --locked

.PHONY: doc
doc: ## Build the docs, private items included
	cargo doc --workspace --all-features --no-deps --document-private-items --locked

.PHONY: test
test: ## Run the integration tests in tests/spec
	cargo test --workspace --all-features --locked

.PHONY: deny
deny: ## Check advisories, licenses and crate sources (needs cargo-deny)
	cargo deny --locked check

.PHONY: format
format: ## Reformat the code in place
	cargo fmt --all
