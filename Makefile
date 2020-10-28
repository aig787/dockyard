VERSION=$(shell cargo run -- --version | cut -d ' ' -f 2)
SHA=$(shell git rev-parse HEAD)

release:
	cargo build --release

docker:
	docker build -t dockyard:${VERSION} -t dockyard:${SHA} .