STORE ?= http://localhost:8079/zarr-test/2079_R1.zarr
BIND ?= 127.0.0.1:8078

build:
	mkdir -p app/assets
	cd app && trunk build --release
	cargo build --release

dev-app:
	mkdir -p app/assets
	cd app && trunk watch

dev-server:
	cargo watch -w server -w src -x "run -- --store $(STORE) --bind $(BIND)"

serve: build
	cargo run -- --store $(STORE) --bind $(BIND)

clean:
	cargo clean
	rm -rf dist/*

.PHONY: build serve clean dev-app dev-server
