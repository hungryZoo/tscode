#!/bin/sh
set -eu

repo="${TSCODE_REPO:-hungryZoo/tscode}"
version="${TSCODE_VERSION:-}"
install_dir="${TSCODE_INSTALL_DIR:-$HOME/.local/bin}"

need() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "tscode installer: missing required command: $1" >&2
        exit 1
    fi
}

curl_stream() {
    curl -fsSL \
        --retry "${TSCODE_CURL_RETRIES:-3}" \
        --retry-delay "${TSCODE_CURL_RETRY_DELAY:-2}" \
        --connect-timeout "${TSCODE_CURL_CONNECT_TIMEOUT:-20}" \
        --max-time "${TSCODE_CURL_MAX_TIME:-300}" \
        --speed-time "${TSCODE_CURL_SPEED_TIME:-30}" \
        --speed-limit "${TSCODE_CURL_SPEED_LIMIT:-1}" \
        "$@"
}

curl_download() {
    download_url="$1"
    output_path="$2"
    curl -fL \
        --retry "${TSCODE_CURL_RETRIES:-3}" \
        --retry-delay "${TSCODE_CURL_RETRY_DELAY:-2}" \
        --connect-timeout "${TSCODE_CURL_CONNECT_TIMEOUT:-20}" \
        --max-time "${TSCODE_CURL_MAX_TIME:-300}" \
        --speed-time "${TSCODE_CURL_SPEED_TIME:-30}" \
        --speed-limit "${TSCODE_CURL_SPEED_LIMIT:-1}" \
        "$download_url" \
        -o "$output_path"
}

detect_target() {
    os="$(uname -s | tr '[:upper:]' '[:lower:]')"
    arch="$(uname -m | tr '[:upper:]' '[:lower:]')"
    exe=""
    archive_ext="tar.gz"

    case "$os" in
        darwin)
            case "$arch" in
                x86_64|amd64) target="x86_64-apple-darwin" ;;
                arm64|aarch64) target="aarch64-apple-darwin" ;;
                *) echo "tscode installer: unsupported macOS architecture: $arch" >&2; exit 1 ;;
            esac
            ;;
        linux)
            case "$arch" in
                x86_64|amd64) target="x86_64-unknown-linux-musl" ;;
                arm64|aarch64) target="aarch64-unknown-linux-musl" ;;
                armv7l|armv7|armhf) target="armv7-unknown-linux-gnueabihf" ;;
                *) echo "tscode installer: unsupported Linux architecture: $arch" >&2; exit 1 ;;
            esac
            ;;
        mingw*|msys*|cygwin*)
            exe=".exe"
            archive_ext="zip"
            case "$arch" in
                x86_64|amd64) target="x86_64-pc-windows-msvc" ;;
                arm64|aarch64) target="aarch64-pc-windows-msvc" ;;
                *) echo "tscode installer: unsupported Windows architecture: $arch" >&2; exit 1 ;;
            esac
            ;;
        *)
            echo "tscode installer: unsupported OS: $os" >&2
            exit 1
            ;;
    esac
}

resolve_version() {
    if [ -n "$version" ] && [ "$version" != "latest" ]; then
        return
    fi

    need curl
    need awk
    version="$(curl_stream "https://api.github.com/repos/$repo/releases?per_page=100" \
        | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' \
        | awk '
            /^v[0-9]+\.[0-9]+\.[0-9]+(-pre\.[0-9]+)?$/ {
                raw = $0
                version = raw
                sub(/^v/, "", version)
                split(version, parts, /[.-]/)
                stable = 1
                pre = 0
                if (parts[4] == "pre") {
                    stable = 0
                    pre = parts[5] + 0
                }
                key = sprintf("%09d%09d%09d%09d%09d", parts[1] + 0, parts[2] + 0, parts[3] + 0, stable, pre)
                if (key > best) {
                    best = key
                    tag = raw
                }
            }
            END {
                if (tag != "") {
                    print tag
                }
            }
        ')"

    if [ -z "$version" ]; then
        echo "tscode installer: could not find a GitHub release for $repo" >&2
        exit 1
    fi
}

download_and_install() {
    need curl
    need mkdir
    need install
    need find

    tmp="$(mktemp -d 2>/dev/null || mktemp -d -t tscode)"
    trap 'rm -rf "$tmp"' EXIT INT TERM

    asset="tscode-${version}-${target}.${archive_ext}"
    url="https://github.com/$repo/releases/download/$version/$asset"
    archive="$tmp/$asset"

    echo "Downloading $url"
    curl_download "$url" "$archive"

    case "$archive_ext" in
        tar.gz)
            need tar
            tar -xzf "$archive" -C "$tmp"
            ;;
        zip)
            if command -v unzip >/dev/null 2>&1; then
                unzip -q "$archive" -d "$tmp"
            elif command -v powershell.exe >/dev/null 2>&1; then
                powershell.exe -NoProfile -Command "Expand-Archive -Force '$archive' '$tmp'"
            else
                echo "tscode installer: need unzip or powershell.exe for Windows archives" >&2
                exit 1
            fi
            ;;
    esac

    bin_path="$(find "$tmp" -type f -name "tscode$exe" | head -n 1)"
    if [ -z "$bin_path" ]; then
        echo "tscode installer: archive did not contain tscode$exe" >&2
        exit 1
    fi

    mkdir -p "$install_dir"
    install -m 755 "$bin_path" "$install_dir/tscode$exe"

    echo "Installed tscode to $install_dir/tscode$exe"
    case ":$PATH:" in
        *":$install_dir:"*) ;;
        *) echo "Add $install_dir to PATH to run 'tscode' from anywhere." ;;
    esac
}

detect_target
resolve_version
download_and_install
