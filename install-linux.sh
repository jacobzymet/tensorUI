#!/bin/sh
# Tensor installer for Linux.
# Usage: curl -fsSL https://github.com/jacobzymet/tensorUI/releases/latest/download/install-linux.sh | sh
set -eu
(set -o pipefail) 2>/dev/null && set -o pipefail

REPO="jacobzymet/tensorUI"
GITHUB="https://github.com/${REPO}"
BIN_NAME="tensor"

usage() {
  cat <<'EOF'
Install Tensor from GitHub Releases (Linux).

Usage:
  install-linux.sh [options]
  curl -fsSL https://github.com/jacobzymet/tensorUI/releases/latest/download/install-linux.sh | sh -s -- [options]

Options:
  --version, -v <ver>  Release to install (default: latest)
  --dir, -d <path>     Install directory (default: ~/.local/bin)
  --no-path            Skip PATH setup hints
  -h, --help           Show this help

Environment:
  TENSOR_VERSION       Same as --version
  TENSOR_INSTALL_DIR   Same as --dir
  GITHUB_TOKEN         Optional token if GitHub rate-limits you
EOF
}

err() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

info() {
  printf '%s\n' "$*"
}

github_curl() {
  if [ -n "${GITHUB_TOKEN:-}" ]; then
    curl --connect-timeout 20 --retry 3 --retry-delay 1 -H "User-Agent: tensor-install" -H "Authorization: Bearer ${GITHUB_TOKEN}" "$@"
  else
    curl --connect-timeout 20 --retry 3 --retry-delay 1 -H "User-Agent: tensor-install" "$@"
  fi
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || err "missing required command: $1"
}

version="${TENSOR_VERSION:-}"
install_dir="${TENSOR_INSTALL_DIR:-}"
no_path=0

while [ $# -gt 0 ]; do
  case "$1" in
    --version|-v)
      [ $# -ge 2 ] || err "--version requires a value"
      version=$2
      shift 2
      ;;
    --dir|-d)
      [ $# -ge 2 ] || err "--dir requires a value"
      install_dir=$2
      shift 2
      ;;
    --no-path)
      no_path=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      err "unknown argument: $1"
      ;;
  esac
done

os=$(uname -s)
[ "$os" = Linux ] || err "this installer is for Linux. On macOS run:
  curl -fsSL https://github.com/jacobzymet/tensorUI/releases/latest/download/install-macos.sh | sh"

if [ -f /etc/alpine-release ]; then
  err "Alpine/musl builds are not published. Use a glibc distribution, or build from source."
fi

need_cmd curl
need_cmd tar
need_cmd uname
need_cmd mktemp

[ -n "${HOME:-}" ] || err "HOME is not set"
if [ -z "$install_dir" ]; then
  install_dir="${HOME}/.local/bin"
fi

arch=$(uname -m)
case "$arch" in
  x86_64|amd64) arch=x86_64 ;;
  aarch64|arm64) arch=aarch64 ;;
  *) err "unsupported architecture: $(uname -m). Releases cover x86_64 and aarch64." ;;
esac
target="${arch}-linux-gnu"

if [ -z "$version" ]; then
  info "Looking up the latest Tensor release..."
  final=$(github_curl -fsSLI --max-time 30 -o /dev/null -w '%{url_effective}' "${GITHUB}/releases/latest") || err "could not resolve the latest release"
  final=$(printf '%s' "$final" | tr -d '\r')
  tag=${final##*/}
else
  tag=$version
fi
case "$tag" in
  v*) ;;
  *) tag="v${tag}" ;;
esac
version=${tag#v}
[ -n "$version" ] || err "could not determine a release version"
info "Installing Tensor ${version} (${target})..."

WORKDIR=$(mktemp -d "${TMPDIR:-/tmp}/tensor-install.XXXXXX")
cleanup() {
  if [ -n "${WORKDIR:-}" ] && [ -d "$WORKDIR" ]; then
    rm -rf "$WORKDIR"
  fi
}
trap cleanup EXIT INT HUP

asset=""
url=""
archive="${WORKDIR}/tensor.tgz"
base="${GITHUB}/releases/download/${tag}"
for prefix in tensor tensorui; do
  candidate="${prefix}-${version}-${target}.tar.gz"
  candidate_url="${base}/${candidate}"
  if github_curl -fsSLI --max-time 30 -o /dev/null "$candidate_url" 2>/dev/null; then
    asset=$candidate
    url=$candidate_url
    break
  fi
done
[ -n "$asset" ] || err "no Linux archive found for ${tag} (${target})"
info "Downloading ${asset}..."
github_curl -fL --max-time 600 -o "$archive" "$url"

sums="${WORKDIR}/SHA256SUMS"
if github_curl -fsSL --max-time 30 -o "$sums" "${base}/SHA256SUMS" 2>/dev/null; then
  expected=$(awk -v f="$asset" '$2 == f || $2 == "*"f || $2 == "./"f { print $1; exit }' "$sums")
  [ -n "$expected" ] || err "SHA256SUMS does not list ${asset}"
  actual=""
  if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$archive" | awk '{ print $1 }')
  elif command -v shasum >/dev/null 2>&1; then
    actual=$(shasum -a 256 "$archive" | awk '{ print $1 }')
  else
    err "need sha256sum or shasum to verify ${asset}"
  fi
  [ "$actual" = "$expected" ] || err "checksum mismatch for ${asset}"
  info "Checksum verified."
fi

extract="${WORKDIR}/extract"
mkdir "$extract"
tar -xzf "$archive" -C "$extract"

src=""
for candidate in "$extract"/*/tensor "$extract"/tensor "$extract"/*/tensorui "$extract"/tensorui; do
  if [ -f "$candidate" ]; then
    src=$candidate
    break
  fi
done
[ -n "$src" ] || err "archive did not contain a tensor binary"

mkdir -p "$install_dir"
dest="${install_dir}/${BIN_NAME}"
if command -v install >/dev/null 2>&1; then
  install -m 755 "$src" "$dest"
else
  cp "$src" "$dest"
  chmod 755 "$dest"
fi

info "Installed ${BIN_NAME} ${version} to ${dest}"

if "$dest" --version >/dev/null 2>&1; then
  "$dest" --version
fi

webkit_found=0
for lib in \
  /usr/lib/libwebkit2gtk-4.1.so.0 \
  /usr/lib64/libwebkit2gtk-4.1.so.0 \
  /usr/lib/*/libwebkit2gtk-4.1.so.0; do
  if [ -e "$lib" ]; then
    webkit_found=1
    break
  fi
done
if [ "$webkit_found" -eq 0 ]; then
  info ""
  info "WebKitGTK 4.1 was not found. Install it to use the desktop window, or run: tensor --browser"
  if [ -f /etc/os-release ]; then
    # shellcheck disable=SC1091
    . /etc/os-release
  fi
  case "${ID:-} ${ID_LIKE:-}" in
    *debian*|*ubuntu*)
      info "  sudo apt install libwebkit2gtk-4.1-0 libgtk-3-0"
      ;;
    *fedora*|*rhel*|*centos*)
      info "  sudo dnf install webkit2gtk4.1 gtk3"
      ;;
    *arch*)
      info "  sudo pacman -S webkit2gtk-4.1 gtk3"
      ;;
    *suse*)
      info "  sudo zypper install libwebkit2gtk-4_1-0 libgtk-3-0"
      ;;
    *)
      info "  Install WebKitGTK 4.1 with your package manager."
      ;;
  esac
fi

onpath=0
case ":${PATH}:" in
  *:"${install_dir}":*) onpath=1 ;;
esac
if [ "$no_path" -eq 0 ] && [ "$onpath" -eq 0 ]; then
  info ""
  info "${install_dir} is not on PATH. Add it for the current shell:"
  info "  export PATH=\"${install_dir}:\$PATH\""
  case "${SHELL:-}" in
    */zsh)
      info "Then persist it in ~/.zshrc."
      ;;
    */fish)
      info "Or in fish:  fish_add_path ${install_dir}"
      ;;
    *)
      info "Then persist it in ~/.bashrc or ~/.profile."
      ;;
  esac
fi

info ""
info "Launch Tensor with:  tensor"
info "Browser UI:          tensor --browser"
info "Headless:            tensor --headless"
