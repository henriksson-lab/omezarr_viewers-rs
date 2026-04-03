BUCKET ?= zarr-test
ENDPOINT ?= http://localhost:3900
REGION ?= us-east-1
PREFIX ?= 
ACCESS_KEY ?= GK5a4c114f1bc5752d05e1bddd
SECRET_KEY ?= edea2705850ebf8d4d995cb3ef8293e6b677b0faddccb95fb5a376ba67bbe477
BIND ?= 127.0.0.1:8078

build:
	mkdir -p app/assets
	cd app && trunk build --release
	cargo build --release

dev-app:
	mkdir -p app/assets
	cd app && trunk watch

PREFIX_ARG = $(if $(PREFIX),--prefix $(PREFIX),)

dev-server:
	cargo watch -w server -w src -x "run -- --bucket $(BUCKET) --endpoint $(ENDPOINT) --region $(REGION) $(PREFIX_ARG) --access-key $(ACCESS_KEY) --secret-key $(SECRET_KEY) --bind $(BIND)"

serve: build
	cargo run -- --bucket $(BUCKET) --endpoint $(ENDPOINT) --region $(REGION) $(PREFIX_ARG) --access-key $(ACCESS_KEY) --secret-key $(SECRET_KEY) --bind $(BIND)

clean:
	cargo clean
	rm -rf dist/*

.PHONY: build serve clean dev-app dev-server
