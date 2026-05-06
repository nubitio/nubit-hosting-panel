#!/bin/sh
# hostingctl installer — Linux servers only
# Usage: curl -fsSL https://github.com/nubitio/nubit-hosting-panel/releases/latest/download/install.sh | sh
set -e

REPO="${HOSTINGCTL_REPO:-nubitio/nubit-hosting-panel}"
BINARY="hostingctl"
PREFIX=""
TOKEN="${HOSTINGCTL_GITHUB_TOKEN:-${GITHUB_TOKEN:-}}"

curl_auth() {
  if [ -n "${TOKEN}" ]; then
    curl -H "Authorization: Bearer ${TOKEN}" -H "Accept: application/vnd.github+json" "$@"
  else
    curl "$@"
  fi
}

while [ $# -gt 0 ]; do
  case "$1" in
    --prefix=*) PREFIX="${1#--prefix=}" ;;
    --prefix) shift; PREFIX="$1" ;;
  esac
  shift
done

OS="$(uname -s)"
ARCH="$(uname -m)"

if [ "${OS}" != "Linux" ]; then
  echo "error: hostingctl installer supports Linux servers only; detected ${OS}" >&2
  exit 1
fi

case "${ARCH}" in
  x86_64) TARGET="x86_64-unknown-linux-gnu" ;;
  aarch64|arm64) TARGET="aarch64-unknown-linux-gnu" ;;
  *) echo "error: unsupported Linux architecture: ${ARCH}" >&2; exit 1 ;;
esac

if [ -n "${PREFIX}" ]; then
  BIN_DIR="${PREFIX}/bin"
else
  BIN_DIR="/usr/local/bin"
fi

if [ -n "${HOSTINGCTL_VERSION:-}" ]; then
  VERSION="${HOSTINGCTL_VERSION}"
else
  echo "Fetching latest release…"
  VERSION="$(curl_auth -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
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
curl_auth -fsSL "${URL}" -o "${TMPDIR}/${ARCHIVE}"

echo "Verifying checksum…"
curl_auth -fsSL "${URL}.sha256" -o "${TMPDIR}/${ARCHIVE}.sha256"
(cd "${TMPDIR}" && sha256sum -c "${ARCHIVE}.sha256")

echo "Installing to ${BIN_DIR}…"
tar -xzf "${TMPDIR}/${ARCHIVE}" -C "${TMPDIR}"
SRC_BIN="${TMPDIR}/hostingctl-${VERSION}-${TARGET}/${BINARY}"
INSTALL_TMP="${BIN_DIR}/.${BINARY}.tmp.$$"

if [ -w "${BIN_DIR}" ] || { [ ! -e "${BIN_DIR}" ] && [ -w "$(dirname "${BIN_DIR}")" ]; }; then
  mkdir -p "${BIN_DIR}"
  cp "${SRC_BIN}" "${INSTALL_TMP}"
  chmod +x "${INSTALL_TMP}"
  mv -f "${INSTALL_TMP}" "${BIN_DIR}/${BINARY}"
elif command -v sudo >/dev/null 2>&1; then
  sudo mkdir -p "${BIN_DIR}"
  sudo cp "${SRC_BIN}" "${INSTALL_TMP}"
  sudo chmod +x "${INSTALL_TMP}"
  sudo mv -f "${INSTALL_TMP}" "${BIN_DIR}/${BINARY}"
else
  echo "error: ${BIN_DIR} is not writable and sudo is unavailable" >&2
  echo "       retry with --prefix ~/.local or run as root" >&2
  exit 1
fi

echo "✓ hostingctl ${VERSION} installed to ${BIN_DIR}/${BINARY}"
echo "Get started: hostingctl init && hostingctl tui"
