#!/usr/bin/env bash
# Arca CI local (T01). El ping musl es necesario para el E2E de exec-native
# (apps de prueba ESTÁTICAS: el sandbox bloquea openat).
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
if rustup target list --installed | grep -q x86_64-unknown-linux-musl; then
  cargo build -p arca-rt --bin arca-ping --target x86_64-unknown-linux-musl
else
  echo "WARN: target musl no instalado (e2e de exec-native se saltará)"
fi
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
# selftest del demo F3a: render→publish→seqlock→lectura sin teléfono
cargo build -p devapp-demo
./target/debug/devapp-demo --selftest
python3 scripts/check-graphs.py
echo "CI OK"
