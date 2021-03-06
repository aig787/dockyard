# dockyard

[![license](https://img.shields.io/github/license/aig787/dockyard)](https://opensource.org/licenses/MIT)
[![travis-ci](https://travis-ci.org/aig787/dockyard.svg?branch=master)](https://travis-ci.org/github/aig787/dockyard)
[![dockerhub](https://img.shields.io/docker/v/aig787/dockyard?label=dockerhub&sort=semver)](https://hub.docker.com/r/aig787/dockyard)
[![crates.io](https://img.shields.io/crates/v/dockyard)](https://crates.io/crates/dockyard)
[![docs](https://docs.rs/dockyard/badge.svg)](https://docs.rs/dockyard/)
[![Coverage Status](https://coveralls.io/repos/github/aig787/dockyard/badge.svg?branch=agriffin/code-coverage)](https://coveralls.io/github/aig787/dockyard?branch=agriffin/code-coverage)

## Dockyard: Back up and restore Docker Resources

Dockyard can back up Docker volumes and containers (automatically backing up mounted volumes).

### Install

```shell
cargo install dockyard
```

### Usage

#### docker-compose
```yaml
---
version: "2.1"
services:
  dockyard:
    image: aig787/dockyard
    container_name: dockyard
    environment:
      - OUTPUT_TYPE=directory # or volume
      - OUTPUT=/backups # or volume name
      - ARGS="" # optional additional arguments
    restart: unless-stopped
```

#### docker cli

```shell
docker run -d \
--name=dockyard \
-e OUTPUT_TYPE=directory # or volume \
-e OUTPUT=/backups # or volume name \
-e ARGS="" # optional additional arguments \
--restart unless-stopped \
aig787/dockyard
```

#### Shell
```shell
# Back up volume to directory
dockyard backup volume <volume> <backup-directory>

# Back up volume to volume
dockyard backup volume <volume> <backup-volume> --output-type volume

# Back up container and all volumes
dockyard backup container <container> <backup-directory>

# Back up container and specific volumes
dockyard backup container <container> <backup-directory> --volumes <volume1> <volume2>

# Restore volume
dockyard restore volume <relative_archive_path> <backup-directory> <volume>

# Restore container
dockyard restore container <relative-backup-file> <backup-directory> <container>

# Monitor and back up all containers
dockyard watch --exclude-volumes <volumes> --exclude-containers <containers>
```

### Example Back Up and Restore
```shell
❯ dockyard backup container nginx /tmp
2020-10-22 16:09:02,555 INFO  [dockyard::backup] Backing up directory /host_mnt/Users/aig787/test to dockyard/binds/:volume1 on /tmp
2020-10-22 16:09:05,600 INFO  [dockyard::backup] Successfully backed up to dockyard/binds/:volume1/2020-10-22T23:09:02.555772+00:00.tgz
2020-10-22 16:09:05,600 INFO  [dockyard::backup] Backing up volume hello to dockyard/volumes/hello
2020-10-22 16:09:10,960 INFO  [dockyard::backup] Successfully backed up to dockyard/volumes/hello/2020-10-22T23:09:05.600782+00:00.tgz
2020-10-22 16:09:10,960 INFO  [dockyard::backup] Writing container backup file dockyard/containers/nginx/2020-10-22T23:09:10.960344+00:00.json
2020-10-22 16:09:16,425 INFO  [dockyard] Successfully backed up container nginx to dockyard/containers/nginx/2020-10-22T23:09:10.960344+00:00.json


❯ dockyard restore container dockyard/containers/nginx/2020-10-22T23:09:10.960344+00:00.json /tmp nginx-restore
2020-10-22 16:10:37,371 INFO  [dockyard::restore] Restoring container nginx-restore from dockyard/containers/nginx/2020-10-22T23:09:10.960344+00:00.json
2020-10-22 16:10:40,356 INFO  [dockyard::restore] Restoring directory /host_mnt/Users/aig787/test from dockyard/binds/:volume1/2020-10-22T23:09:02.555772+00:00.tgz
2020-10-22 16:10:46,127 INFO  [dockyard::restore] Successfully restored mount /host_mnt/Users/aig787/test
2020-10-22 16:10:46,127 INFO  [dockyard::restore] Restoring volume hello from dockyard/volumes/hello/2020-10-22T23:09:05.600782+00:00.tgz
2020-10-22 16:10:51,412 INFO  [dockyard::restore] Successfully restored mount hello
2020-10-22 16:10:51,485 INFO  [dockyard::restore] Successfully restored container nginx-restore
```
### Building the docs
```shell
make docs
```

### Building the container
```shell
make docker
```

### Running Tests
```shell
cargo test
```

License: MIT
