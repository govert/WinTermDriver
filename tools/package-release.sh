#!/usr/bin/env bash
#
# Build WinTermDriver in release mode and package binaries into a versioned zip.
#
# Usage:
#   ./tools/package-release.sh [--version <version>]
#
# If --version is not given, reads from workspace Cargo.toml.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

# Parse arguments
VERSION=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --version)
            VERSION="$2"
            shift 2
            ;;
        *)
            echo "Unknown argument: $1" >&2
            exit 1
            ;;
    esac
done

# Extract version from workspace Cargo.toml if not provided
if [[ -z "$VERSION" ]]; then
    VERSION=$(grep -A2 '^\[workspace\.package\]' Cargo.toml | grep 'version' | sed 's/.*"\(.*\)".*/\1/')
    if [[ -z "$VERSION" ]]; then
        echo "Error: could not extract version from Cargo.toml" >&2
        exit 1
    fi
fi

ARCHIVE_NAME="wtd-${VERSION}-windows-x86_64"
STAGING_DIR="target/package/${ARCHIVE_NAME}"
BINARIES=(
    "target/release/wtd-host.exe"
    "target/release/wtd-ui.exe"
    "target/release/wtd.exe"
)

echo "Building WinTermDriver v${VERSION} in release mode..."
cargo build --release --bin wtd-host --bin wtd-ui --bin wtd

echo "Collecting artifacts into ${STAGING_DIR}..."
rm -rf "$STAGING_DIR"
mkdir -p "$STAGING_DIR"

for bin in "${BINARIES[@]}"; do
    if [[ ! -f "$bin" ]]; then
        echo "Error: expected binary not found: $bin" >&2
        exit 1
    fi
    cp "$bin" "$STAGING_DIR/"
done

# Bundle README and LICENSE
cp README.md "$STAGING_DIR/"
cp LICENSE.md "$STAGING_DIR/"

echo "Creating archive..."
mkdir -p target/package
cd target/package
rm -f "${ARCHIVE_NAME}.zip"

# Use PowerShell's Compress-Archive (available on all Windows 10+)
powershell.exe -NoProfile -Command \
    "Compress-Archive -Path '${ARCHIVE_NAME}\\*' -DestinationPath '${ARCHIVE_NAME}.zip' -Force"

cd "$REPO_ROOT"

ZIP_PATH="target/package/${ARCHIVE_NAME}.zip"
echo ""
echo "Package created: ${ZIP_PATH}"
echo "Contents:"
powershell.exe -NoProfile -Command \
    "Get-ChildItem '${STAGING_DIR}' | Format-Table Name, Length -AutoSize"
