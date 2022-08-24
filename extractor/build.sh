#!/bin/sh

docker run --rm -v "$PWD":/usr/src/myapp -w /usr/src/myapp rust cargo build --release
