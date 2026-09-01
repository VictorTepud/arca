# Arca — tareas canónicas (install: cargo install just)
default: fmt-check clippy test graphs

# Formato
fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

clippy:
    cargo clippy --workspace --all-targets -- -D warnings

test:
    cargo test --workspace

graphs:
    python3 scripts/check-graphs.py

# App de prueba estática para el E2E de exec-native
musl-ping:
    rustup target add x86_64-unknown-linux-musl
    cargo build -p arca-rt --bin arca-ping --target x86_64-unknown-linux-musl

# E2E del backend nativo (spawn→handshake→ping→kill; seccomp real)
e2e-native: musl-ping
    cargo test -p arca-exec-native --test e2e -- --nocapture

# CI completo sin `just`: scripts/ci.sh
ci: default

# CLI de empaquetado (T09)
pk *args:
    cargo run -p arca-tools-pk -- {{args}}

# Gate F0 en dispositivo (T02, requiere NDK + adb)
probe:
    @echo "T02 → docs/09-build-deepin.md §6: cargo ndk + gradle host-probe"
