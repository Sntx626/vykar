#!/usr/bin/env bash
set -euo pipefail

# Cloudflare Pages dashboard build command:
#   bash scripts/cloudflare-pages-build.sh

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TOOLS_ROOT="${ROOT_DIR}/.cloudflare-tools"
TOOLS_BIN="${TOOLS_ROOT}/bin"

MDBOOK_VERSION="${MDBOOK_VERSION:-0.5.0}"
MDBOOK_MERMAID_VERSION="${MDBOOK_MERMAID_VERSION:-0.17.0}"

export PATH="${TOOLS_BIN}:${HOME}/.cargo/bin:${PATH}"

tool_version() {
    local bin="$1"
    "$bin" --version 2>/dev/null | awk 'NR == 1 { print $2 }'
}

have_tool_version() {
    local bin="$1"
    local want="$2"

    if ! command -v "$bin" >/dev/null 2>&1; then
        return 1
    fi

    local got
    got="$(tool_version "$bin")"
    got="${got#v}"
    [ "$got" = "$want" ]
}

ensure_rust_toolchain() {
    if command -v cargo >/dev/null 2>&1; then
        return 0
    fi

    if ! command -v curl >/dev/null 2>&1; then
        echo "error: curl is required to install Rust for the Pages docs build" >&2
        exit 1
    fi

    curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs \
        | sh -s -- -y --profile minimal --default-toolchain stable

    export PATH="${HOME}/.cargo/bin:${PATH}"
}

install_tool() {
    local bin="$1"
    local crate="$2"
    local version="$3"

    if have_tool_version "$bin" "$version"; then
        return 0
    fi

    ensure_rust_toolchain
    cargo install --root "${TOOLS_ROOT}" --locked --force --version "$version" "$crate"
}

install_tool mdbook mdbook "${MDBOOK_VERSION}"
install_tool mdbook-mermaid mdbook-mermaid "${MDBOOK_MERMAID_VERSION}"

cd "${ROOT_DIR}"
make docs-build
