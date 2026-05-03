#!/usr/bin/env bash
# Build librclone (rclone as a C-shared library) for the current host.
#
# Output: src-tauri/lib/<rust-target-triple>/librclone.{so,dll,dylib} + librclone.h
#
# Required tools:
#   - Go >= 1.21      (https://go.dev/dl/)
#   - A working C compiler with CGO (mandatory because librclone is c-shared):
#       Linux:    gcc
#       Windows:  mingw-w64 / msys2 ("pacman -S mingw-w64-x86_64-gcc"), or run via WSL
#       macOS:    Xcode CLT ("xcode-select --install")
#
# Env vars:
#   RCLONE_VERSION  git tag/branch to build (default: v1.69.0)
#   RCLONE_SRC      pre-existing rclone source dir (skip git clone)

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RCLONE_VERSION="${RCLONE_VERSION:-v1.69.0}"
SRC_DIR="${RCLONE_SRC:-$ROOT/build/rclone-src}"
OUT_BASE="$ROOT/src-tauri/lib"

case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*)
    TRIPLE="x86_64-pc-windows-gnu"
    LIB_NAME="librclone.dll"
    ;;
  Linux)
    if [[ "$(uname -m)" == "aarch64" ]]; then
      TRIPLE="aarch64-unknown-linux-gnu"
    else
      TRIPLE="x86_64-unknown-linux-gnu"
    fi
    LIB_NAME="librclone.so"
    ;;
  Darwin)
    if [[ "$(uname -m)" == "arm64" ]]; then
      TRIPLE="aarch64-apple-darwin"
    else
      TRIPLE="x86_64-apple-darwin"
    fi
    LIB_NAME="librclone.dylib"
    ;;
  *)
    echo "[!] Unsupported host: $(uname -s)" >&2
    exit 1
    ;;
esac

OUT_DIR="$OUT_BASE/$TRIPLE"

echo "[*] target triple : $TRIPLE"
echo "[*] output dir    : $OUT_DIR"
echo "[*] rclone version: $RCLONE_VERSION"

if ! command -v go >/dev/null 2>&1; then
  echo "[!] go not found on PATH — install Go from https://go.dev/dl/" >&2
  exit 1
fi
echo "[*] go: $(go version)"

if ! command -v gcc >/dev/null 2>&1; then
  echo "[!] gcc not found on PATH — librclone requires CGO" >&2
  echo "    linux:   apt install build-essential / dnf install gcc"
  echo "    windows: install mingw-w64 (msys2) and add to PATH"
  echo "    macos:   xcode-select --install"
  exit 1
fi
echo "[*] cc: $(gcc --version | head -1)"

# Get rclone source
if [[ -n "${RCLONE_SRC:-}" ]]; then
  if [[ ! -d "$SRC_DIR" ]]; then
    echo "[!] RCLONE_SRC=$SRC_DIR does not exist" >&2
    exit 1
  fi
  echo "[*] using existing source at $SRC_DIR"
elif [[ ! -d "$SRC_DIR/.git" ]]; then
  mkdir -p "$(dirname "$SRC_DIR")"
  echo "[*] cloning rclone $RCLONE_VERSION into $SRC_DIR ..."
  git clone --depth 1 --branch "$RCLONE_VERSION" \
    https://github.com/rclone/rclone.git "$SRC_DIR"
else
  echo "[*] reusing $SRC_DIR (set RCLONE_VERSION env to switch tags)"
  (cd "$SRC_DIR" \
    && git fetch --depth 1 origin "$RCLONE_VERSION" \
    && git checkout --quiet FETCH_HEAD)
fi

# Build
mkdir -p "$OUT_DIR"
echo "[*] building $LIB_NAME ..."
(  cd "$SRC_DIR"
  CGO_ENABLED=1 go build \
    -buildmode=c-shared \
    -trimpath \
    -ldflags="-s -w" \
    -o "$OUT_DIR/$LIB_NAME" \
    ./librclone
)

echo "[*] done"
ls -lh "$OUT_DIR/" | awk 'NR>1 {print "    " $0}'

# The header is emitted next to the lib by `go build -buildmode=c-shared`.
if [[ ! -f "$OUT_DIR/librclone.h" ]]; then
  echo "[!] librclone.h not generated — unexpected" >&2
  exit 1
fi
