#!/usr/bin/env bash
set -euo pipefail

repo="${ORBI_INSTALL_REPO:-iivankin/orbit}"
install_dir="${ORBI_INSTALL_DIR:-$HOME/.local/bin}"

uname_s="$(uname -s)"
uname_m="$(uname -m)"

case "$uname_s" in
  Linux)
    os="linux"
    ext="tar.gz"
    ;;
  Darwin)
    os="macos"
    ext="tar.gz"
    ;;
  *)
    echo "unsupported OS: $uname_s" >&2
    exit 1
    ;;
esac

case "$uname_m" in
  x86_64|amd64)
    arch="x86_64"
    ;;
  arm64|aarch64)
    arch="arm64"
    ;;
  *)
    echo "unsupported architecture: $uname_m" >&2
    exit 1
    ;;
esac

asset="orbi-${os}-${arch}.${ext}"

case "$asset" in
  orbi-linux-x86_64.tar.gz|orbi-macos-arm64.tar.gz)
    ;;
  *)
    echo "no published binary for ${os}-${arch}" >&2
    exit 1
    ;;
esac

url="https://github.com/${repo}/releases/latest/download/${asset}"
tmp_dir="$(mktemp -d)"
archive_path="${tmp_dir}/${asset}"

cleanup() {
  rm -rf "$tmp_dir"
}
trap cleanup EXIT

echo "downloading ${url}"
curl -fsSL "$url" -o "$archive_path"

mkdir -p "$install_dir"
tar -xzf "$archive_path" -C "$tmp_dir"
install -m 0755 "${tmp_dir}/orbi" "${install_dir}/orbi"

echo "installed orbi to ${install_dir}/orbi"
echo "make sure ${install_dir} is in PATH"
