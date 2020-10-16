VERSION=$(shell cargo run -- --version | cut -d ' ' -f 2)
SHA=$(shell git rev-parse HEAD)

release:
	cargo build --release

docker:
	DOCKER_BUILDKIT=1 docker build -t dockyard:${VERSION} -t dockyard:${SHA} .