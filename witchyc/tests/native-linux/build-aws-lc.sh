#!/bin/sh
# The AWS-LC FIPS module (aws-lc-fips-sys 0.14.4) for Linux aarch64, built offline from the
# crate's sources in the local cargo cache. Every tool comes from images already on this machine:
# cmake, a C compiler and perl from one (3b-audit-nydus-linux:local), the Go toolchain copied out
# of another (golang:1.27.3-alpine, statically linked). Nothing is downloaded (GOPROXY=off,
# cargo --offline, --network none). One job: the delocator otherwise runs out of the 1.9 GiB VM.
#
#   cid=$(docker create golang:1.27.3-alpine); docker cp -q $cid:/usr/local/go WORK/go; docker rm $cid
#   mkdir -p WORK/proj/src; cp this-dir/aws-lc-probe.toml WORK/proj/Cargo.toml; echo 'fn main() {}' > WORK/proj/src/main.rs
#   docker run --rm --network none --entrypoint sh -v ~/.cargo/registry:/registry:ro \
#     -v WORK/go:/go:ro -v WORK:/lc 3b-audit-nydus-linux:local /lc/build-aws-lc.sh
#
# The archive is .../out/build/artifacts/libaws_lc_fips_0_14_4_crypto.a and the headers .../out/include:
# copy them to PREFIX/lib and PREFIX/include and build witchyc with WITCHY_NATIVE_AWS_LC_ROOT=PREFIX.
set -e
export CARGO_HOME=/cargo PATH=/go/bin:$PATH GOCACHE=/tmp/gocache GOFLAGS=-mod=mod GOTOOLCHAIN=local GOPROXY=off CARGO_TARGET_DIR=/lc/target
mkdir -p /cargo && ln -sfn /registry /cargo/registry
go version; cmake --version | head -1; gcc --version | head -1
cd /lc/proj && cargo generate-lockfile --offline
CMAKE_BUILD_PARALLEL_LEVEL=1 GOMAXPROCS=1 GOGC=25 cargo build --release --offline -j 1
find /lc/target -name 'libaws_lc_fips_0_14_4_crypto.a'
