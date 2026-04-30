#!/usr/bin/fish

alias rust-musl-builder='docker run --rm -it -v "$(pwd)":/home/rust/src ghcr.io/rust-cross/rust-musl-cross:x86_64-musl'
rust-musl-builder cargo build --release ; or exit 1

echo "Compilation succeeded. The binary is located at target/x86_64-unknown-linux-musl/release/cron-rs"