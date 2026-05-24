# Justfile — Air Elt development commands

set shell := ["bash", "-euo", "pipefail", "-c"]

# Show available recipes
default:
    @just --list

msrv := `grep '^channel' rust-toolchain.toml | cut -d'"' -f2 | cut -d. -f1-2`

# ── Build ─────────────────────────────────────────────────────────────

# Build the workspace in release mode
build:
    cargo build --release --workspace

# Clean build artifacts
clean:
    cargo clean

# ── Format & lint ─────────────────────────────────────────────────────

# Format all code
fmt:
    cargo fmt --all

# Check formatting
lint-fmt:
    cargo fmt --all -- --check

# Run clippy
lint-clippy:
    cargo clippy --workspace --all-targets -- -D warnings

# Run cargo-deny license and advisory checks
lint-deny:
    cargo deny check --hide-inclusion-graph

# Check formatting + clippy + cargo-deny
lint: lint-fmt lint-clippy lint-deny

# CI lint pipeline (alias for lint)
ci-lint: lint

# ── Test ──────────────────────────────────────────────────────────────

# Run tests — auto-format first, then nextest if available, otherwise cargo test
test *args: fmt
    #!/bin/sh
    set -eu
    if command -v cargo-nextest >/dev/null 2>&1; then
        cargo nextest run --workspace --all-targets {{ args }}
    else
        cargo test --workspace --all-targets {{ args }}
    fi

# Full pipeline: format → lint → test
test-full: fmt lint test

# ── Check dependencies ────────────────────────────────────────────────

# Minimal prereqs check — Rust only
check-deps-basic:
    #!/bin/sh
    set -eu
    echo "=== Rust toolchain ==="
    if ! command -v rustc >/dev/null 2>&1; then
        echo "ERROR: rustc not found. Install manually: https://rustup.rs"
        exit 1
    fi
    if ! command -v cargo >/dev/null 2>&1; then
        echo "ERROR: cargo not found. Install manually: https://rustup.rs"
        exit 1
    fi
    rust_version="$(rustc --version | grep -oE '[0-9]+\.[0-9]+\.[0-9]+')"
    rust_minor="$(echo "$rust_version" | cut -d. -f2)"
    msrv_minor="$(echo "{{ msrv }}" | cut -d. -f2)"
    if [ "$rust_minor" -lt "$msrv_minor" ]; then
        echo "ERROR: Rust >= {{ msrv }} required, found $rust_version"
        echo "Run: just rust-update"
        exit 1
    fi
    echo "  rustc:     $rust_version"
    echo "  cargo:     $(cargo --version | grep -oE '[0-9]+\.[0-9]+\.[0-9]+')"
    toolchain="$(rustup show active-toolchain 2>/dev/null || echo 'unknown')"
    echo "  toolchain: $toolchain"
    echo "Basic checks passed."

# CI prereqs check — Rust + clang + cargo tools
check-deps-ci: check-deps-basic
    #!/bin/sh
    set -eu
    echo ""
    echo "=== Required tools ==="
    fail=0
    if command -v clang >/dev/null 2>&1; then
        echo "  clang: $(clang --version 2>/dev/null | head -1)"
    else
        echo "  clang: NOT FOUND — install clang (required by .cargo/config.toml)"
        fail=1
    fi
    if command -v cargo-nextest >/dev/null 2>&1; then
        echo "  cargo-nextest: $(cargo nextest --version 2>/dev/null | head -1)"
    else
        echo "  cargo-nextest: NOT FOUND — install: cargo install cargo-nextest --locked"
        fail=1
    fi
    if command -v cargo-deny >/dev/null 2>&1; then
        echo "  cargo-deny: $(cargo deny --version 2>/dev/null | head -1)"
    else
        echo "  cargo-deny: NOT FOUND — install: cargo install cargo-deny --locked"
        fail=1
    fi
    echo ""
    echo "=== Optional ==="
    os="$(uname -s)"
    if [ "$os" = "Darwin" ] && command -v ld64.lld >/dev/null 2>&1; then
        echo "  ld64.lld: $(ld64.lld --version 2>/dev/null | head -1)"
    elif command -v ld.lld >/dev/null 2>&1; then
        echo "  ld.lld: $(ld.lld --version 2>/dev/null | head -1)"
    elif command -v lld-link >/dev/null 2>&1; then
        echo "  lld-link: $(lld-link --version 2>/dev/null | head -1)"
    else
        echo "  lld: NOT FOUND (optional, but .cargo/config.toml expects it for faster linking)"
    fi
    if [ "$fail" -ne 0 ]; then
        echo ""
        echo "Some required tools are missing. Install them and re-run this check."
        exit 1
    fi
    echo ""
    echo "CI checks passed."

# Local dev prereqs — CI + Docker/Podman + uv + rtk
check-deps-local: check-deps-ci
    #!/bin/sh
    set -eu
    echo ""
    echo "=== Local dev tools ==="
    fail=0
    if command -v docker >/dev/null 2>&1; then
        echo "  docker: $(docker --version 2>/dev/null | head -1)"
    elif command -v podman >/dev/null 2>&1; then
        echo "  podman: $(podman --version 2>/dev/null | head -1)"
    else
        echo "  docker/podman: NOT FOUND — install Docker (https://docs.docker.com/get-docker/) or Podman (https://podman.io)"
        fail=1
    fi
    if command -v uv >/dev/null 2>&1; then
        echo "  uv: $(uv --version 2>/dev/null | head -1)"
    else
        echo "  uv: NOT FOUND — install: https://docs.astral.sh/uv/getting-started/installation/"
        fail=1
    fi
    if command -v rtk >/dev/null 2>&1; then
        echo "  rtk: $(rtk --version 2>/dev/null | head -1)"
    else
        echo "  rtk: NOT FOUND — install: cargo install rtk --locked"
        fail=1
    fi
    if [ "$fail" -ne 0 ]; then
        echo ""
        echo "Some local dev tools are missing (see above)."
        exit 1
    fi
    echo ""
    echo "All local dev checks passed."

# ── Install dependencies ──────────────────────────────────────────────

# Install cargo tools (cargo-deny, cargo-nextest)
install-basic:
    #!/bin/sh
    set -eu
    echo "==> Cargo tools"
    if command -v cargo-deny >/dev/null 2>&1; then
        echo "  cargo-deny: already installed ($(cargo deny --version 2>/dev/null | head -1))"
    else
        echo "  Installing cargo-deny..."
        cargo install cargo-deny --locked
    fi
    if command -v cargo-nextest >/dev/null 2>&1; then
        echo "  cargo-nextest: already installed ($(cargo nextest --version 2>/dev/null | head -1))"
    else
        echo "  Installing cargo-nextest..."
        cargo install cargo-nextest --locked
    fi

# Install CI-level dependencies (cargo tools + clang/lld)
install-ci: install-basic
    #!/bin/sh
    set -eu
    echo ""
    echo "==> Compiler tooling (clang + lld)"
    has_clang=false
    has_lld=false
    command -v clang >/dev/null 2>&1 && has_clang=true
    command -v ld64.lld >/dev/null 2>&1 || command -v ld.lld >/dev/null 2>&1 || command -v lld-link >/dev/null 2>&1 && has_lld=true
    if $has_clang && $has_lld; then
        echo "  clang: already installed ($(clang --version 2>/dev/null | head -1))"
        echo "  lld:   already installed"
    else
        os="$(uname -s)"
        case "$os" in
            Darwin)
                if command -v brew >/dev/null 2>&1; then
                    echo "  Installing lld via Homebrew..."
                    brew install lld
                else
                    echo "  WARNING: Homebrew not found. Install lld manually."
                    echo "    https://brew.sh — then run: brew install lld"
                fi
                ;;
            Linux)
                if command -v apk >/dev/null 2>&1; then
                    echo "  Installing clang + lld via apk (Alpine)..."
                    apk add --no-cache clang lld
                elif command -v apt-get >/dev/null 2>&1; then
                    echo "  Installing clang + lld via apt..."
                    sudo apt-get update -qq
                    sudo apt-get install -y -qq clang lld
                elif command -v dnf >/dev/null 2>&1; then
                    echo "  Installing clang + lld via dnf..."
                    sudo dnf install -y clang lld
                elif command -v pacman >/dev/null 2>&1; then
                    echo "  Installing clang + lld via pacman..."
                    sudo pacman -S --noconfirm clang lld
                else
                    echo "  WARNING: Could not detect package manager. Install clang and lld manually."
                fi
                ;;
            MINGW*|MSYS*)
                target="$(rustc -vV 2>/dev/null | grep '^host:' | cut -d' ' -f2)"
                if echo "$target" | grep -q "gnu"; then
                    echo "  Detected MinGW toolchain ($target)"
                    if command -v pacman >/dev/null 2>&1; then
                        echo "  Installing mingw-w64 clang + lld via MSYS2 pacman..."
                        pacman -S --noconfirm mingw-w64-x86_64-clang mingw-w64-x86_64-lld
                    else
                        echo "  WARNING: MSYS2 pacman not found. Install MSYS2 first: https://www.msys2.org"
                        echo "    Then run: pacman -S mingw-w64-x86_64-clang mingw-w64-x86_64-lld"
                    fi
                else
                    echo "  Detected MSVC toolchain ($target) — no extra linker needed."
                fi
                ;;
            *)
                echo "  WARNING: Unsupported OS ($os). Install clang and lld manually."
                ;;
        esac
    fi

# Install local dev dependencies (cargo tools + clang/lld + uv + rtk)
install-local: install-ci
    #!/bin/sh
    set -eu
    echo ""
    echo "==> Local dev tools (uv, rtk)"
    if command -v uv >/dev/null 2>&1; then
        echo "  uv: already installed ($(uv --version 2>/dev/null | head -1))"
    else
        echo "  Installing uv..."
        curl -LsSf https://astral.sh/uv/install.sh | sh
    fi
    if command -v rtk >/dev/null 2>&1; then
        echo "  rtk: already installed ($(rtk --version 2>/dev/null | head -1))"
    else
        echo "  Installing rtk..."
        cargo install rtk --locked
    fi
    echo ""
    echo "Verifying environment..."
    echo ""
    just check-deps-local

# ── Rust toolchain management ─────────────────────────────────────────

# Show the active Rust version
rust-version:
    rustc --version
    cargo --version

# Update Rust to the latest stable, patch all config files, pull deps
rust-update:
    #!/bin/sh
    set -eu
    if ! command -v rustup >/dev/null 2>&1; then
        echo "ERROR: rustup not found. Install Rust manually: https://rustup.rs"
        exit 1
    fi
    old_version="$(grep '^channel' rust-toolchain.toml | cut -d'"' -f2)"
    echo "Current pinned version: $old_version"
    echo ""
    echo "=== Updating to latest stable ==="
    rustup update stable
    new_version="$(rustup run stable rustc --version | grep -oE '[0-9]+\.[0-9]+\.[0-9]+')"
    new_minor="$(echo "$new_version" | cut -d. -f1-2)"
    echo ""
    if [ "$old_version" = "$new_version" ]; then
        echo "Already on latest stable ($new_version). Nothing to update."
        exit 0
    fi
    echo "=== Updating $old_version → $new_version ==="
    old_minor="$(echo "$old_version" | cut -d. -f1-2)"
    sed -i.bak "s/channel = \"$old_version\"/channel = \"$new_version\"/" rust-toolchain.toml && rm rust-toolchain.toml.bak
    echo "  rust-toolchain.toml: $old_version → $new_version"
    sed -i.bak "s/rust-version = \"$old_minor\"/rust-version = \"$new_minor\"/" Cargo.toml && rm Cargo.toml.bak
    echo "  Cargo.toml rust-version: $old_minor → $new_minor"
    echo ""
    echo "=== Installing updated toolchain ==="
    rustup toolchain install "$new_version" --profile minimal --component rustfmt,clippy
    rustup default "$new_version"
    echo ""
    echo "=== Updating cargo dependencies ==="
    cargo update
    echo ""
    echo "Done. New version:"
    rustc --version
    cargo --version
    echo ""
    echo "Review changes: git diff"
