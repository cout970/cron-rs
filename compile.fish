#!/usr/bin/fish

# alias rust-musl-builder='docker run --rm -it -v "$(pwd)":/home/rust/src ghcr.io/rust-cross/rust-musl-cross:x86_64-musl'
rust-musl-builder cargo build --release