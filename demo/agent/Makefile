# demo-agent's own gate. Not part of the gateway's `make check`.

.PHONY: check lint test build fix

check: lint test

lint:
	uv run --locked ruff check
	uv run --locked ruff format --check

# The integration tests start a real gateway, so the binaries come first.
test: build
	uv run --locked pytest

build:
	cd ../.. && cargo build --quiet
	cd .. && cargo build --quiet --workspace

fix:
	uv run --locked ruff check --fix
	uv run --locked ruff format
