.PHONY: help check lint test fix build

help:
	@echo "proveno-gateway: MCP gateway with policy-checked, recorded, replayable"
	@echo "                 tool calls on the proveno runtime"
	@echo
	@echo "  check          CI gate: lint + test"
	@echo "  lint           cargo fmt --check + cargo clippy -D warnings"
	@echo "  test           all tests"
	@echo "  fix            auto-format + apply safe clippy fixes"
	@echo "  build          cargo build"

check: lint test

lint:
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings

test:
	cargo test

fix:
	cargo fmt --all
	cargo clippy --all-targets --fix --allow-dirty --allow-staged

build:
	cargo build
