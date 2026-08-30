BUCKET ?= zarr-test
ENDPOINT ?= http://localhost:3900
REGION ?= us-east-1
PREFIX ?=
ACCESS_KEY ?= GK5a4c114f1bc5752d05e1bddd
SECRET_KEY ?= edea2705850ebf8d4d995cb3ef8293e6b677b0faddccb95fb5a376ba67bbe477
BIND ?= 127.0.0.1:8078
STORE ?=
PROJECT ?=
DEMO ?= /tmp/omezarr-demo

build:
	mkdir -p app/assets
	cd app && trunk build --release
	cargo build --release

# The frontend has no unit tests; it is checked by driving a real browser.
# Needs Chrome and `pip install websocket-client pillow` — see
# tests/browser/README.md. `make build` first: the server serves dist/.
test-browser:
	python3 tests/browser/run.py

dev-app:
	mkdir -p app/assets
	cd app && trunk watch

PREFIX_ARG = $(if $(PREFIX),--prefix $(PREFIX),)
STORE_ARG = $(if $(STORE),--store $(STORE),)
PROJECT_ARG = $(if $(PROJECT),--project $(PROJECT),)
S3_ARGS = --bucket $(BUCKET) --endpoint $(ENDPOINT) --region $(REGION) $(PREFIX_ARG) \
	--access-key $(ACCESS_KEY) --secret-key $(SECRET_KEY)

dev-server:
	cargo watch -w server -w src -x "run -- $(S3_ARGS) --bind $(BIND)"

serve: build
	cargo run --release -- $(S3_ARGS) $(STORE_ARG) $(PROJECT_ARG) --bind $(BIND)

# Open a run directory (or a project file) with no S3 configured:
#   make run PROJECT=/path/to/clearmap-run
run: build
	cargo run --release -- $(PROJECT_ARG) $(STORE_ARG) --bind $(BIND)

# The desktop app. Needs the frontend built first — it is compiled in.
#
# `cargo tauri build` produces installers (AppImage/deb/dmg/msi) and needs
# `cargo install tauri-cli`; the plain build below produces a runnable binary
# and needs nothing extra.
desktop:
	mkdir -p app/assets
	cd app && trunk build --release
	cargo build --release -p omezarr-viewer-desktop

desktop-bundle:
	mkdir -p app/assets
	cd app && trunk build --release
	cd desktop && cargo tauri build

# A synthetic image + labels + three object tables, for developing without an
# acquisition on hand.
demo:
	cargo run --release --bin make_demo -- $(DEMO)
	@echo "now: make run PROJECT=$(DEMO)"

# Clippy caches by crate fingerprint, so an unchanged crate is *not* re-linted
# and a run can pass on stale results — which is how a lint that CI failed on
# passed here first. Touching each crate root forces the lint to actually run.
# `--workspace`, not `-p server`: the shared crate holds the geometry — hit
# testing, holes, the hierarchy rule, vertex editing — and naming one crate is
# how those went untested for as long as they did.
test:
	cargo test --workspace
	@touch src/lib.rs server/src/lib.rs app/src/main.rs desktop/src/main.rs
	cargo clippy --workspace --all-targets -- -D warnings
	cd app && cargo clippy --target wasm32-unknown-unknown -- -D warnings
	cargo fmt --all -- --check

clean:
	cargo clean
	rm -rf dist/*

.PHONY: build serve run desktop desktop-bundle demo test clean dev-app dev-server
