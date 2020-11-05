VERSION=$(shell cargo run -- --version | cut -d ' ' -f 2)
SHA=$(shell git rev-parse HEAD)

all: release docker

clean:
	cargo clean

docs: readme
	cargo doc

readme:
	cargo readme > README.md

test: docker
	cargo test

release:
	cargo build --release

docker:
	docker build -t dockyard:${VERSION} -t dockyard:${SHA} .