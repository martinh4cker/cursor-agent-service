.PHONY: check build run

check:
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets -- -D warnings
	cargo test --workspace --all-targets

build:
	cargo build --release -p cursor-server --bin cursor-server

run:
	cargo run -p cursor-server -- serve --config ./config.yaml
