#!/bin/sh
# hostingctl installer — macOS and Linux
# Usage: curl -fsSL https://github.com/nubitio/nubit-hosting-panel/releases/latest/download/install.sh | sh
set -e

REPO="${HOSTINGCTL_REPO:-nubitio/nubit-hosting-panel}"
BINARY="hostingctl"
PREFIX=""

while [ $# -gt 0 ]; do
  case "$1" in
    --prefix=*) PREFIX="${1#--prefix=}" ;;
    --prefix) shift; PREFIX="$1" ;;
  esac
  shift
done

OS="$(uname -s)"
ARCH="$(uname -m)"
case "${OS}" in
  Linux)
    case "${ARCH}" in
      x86_64) TARGET="x86_64-unknown-linux-gnu" ;;
      aarch64|arm64) TARGET="aarch64-unknown-linux-gnu" ;;
      *) echo "error: unsupported Linux architecture: ${ARCH}" >&2; exit 1 ;;
    esac
    ;;
  Darwin)
    case "${ARCH}" in
      x86_64) TARGET="x86_64-apple-darwin" ;;
      arm64) TARGET="aarch64-apple-darwin" ;;
      *) echo "error: unsupported macOS architecture: ${ARCH}" >&2; exit 1 ;;
    esac
    ;;
  *) echo "error: unsupported operating system: ${OS}" >&2; exit 1 ;;
esac

if [ -n "${PREFIX}" ]; then
  BIN_DIR="${PREFIX}/bin"
else
  BIN_DIR="${HOME}/.local/bin"
fi

if [ -n "${HOSTINGCTL_VERSION:-}" ]; then
  VERSION="${HOSTINGCTL_VERSION}"
else
  echo "Fetching latest release…"
  VERSION="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
    | grep '"tag_name"' \
    | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')"
fi

if [ -z "${VERSION}" ]; then
  echo "error: could not determine latest version. Set HOSTINGCTL_VERSION to override." >&2
  exit 1
fi

ARCHIVE="hostingctl-${VERSION}-${TARGET}.tar.gz"
URL="https://github.com/${REPO}/releases/download/${VERSION}/${ARCHIVE}"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "${TMPDIR}"' EXIT

echo "Downloading hostingctl ${VERSION} for ${TARGET}…"
curl -fsSL "${URL}" -o "${TMPDIR}/${ARCHIVE}"

if command -v sha256sum >/dev/null 2>&1 || command -v shasum >/dev/null 2>&1; then
  echo "Verifying checksum…"
  curl -fsSL "${URL}.sha256" -o "${TMPDIR}/${ARCHIVE}.sha256"
  (cd "${TMPDIR}" && sha256sum -c "${ARCHIVE}.sha256" 2>/dev/null) || \
  (cd "${TMPDIR}" && shasum -a 256 -c "${ARCHIVE}.sha256")
fi

echo "Installing to ${BIN_DIR}…"
tar -xzf "${TMPDIR}/${ARCHIVE}" -C "${TMPDIR}"
mkdir -p "${BIN_DIR}"
cp "${TMPDIR}/hostingctl-${VERSION}-${TARGET}/${BINARY}" "${BIN_DIR}/${BINARY}"
chmod +x "${BIN_DIR}/${BINARY}"

echo "✓ hostingctl ${VERSION} installed to ${BIN_DIR}/${BINARY}"
case ":${PATH}:" in
  *":${BIN_DIR}:"*) ;;
  *) echo "Add to PATH: export PATH=\"${BIN_DIR}:\$PATH\"" ;;
esac

echo "Get started: hostingctl init && hostingctl tui"
