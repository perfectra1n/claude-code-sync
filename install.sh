#!/bin/sh
# claude-code-sync installer for Linux and macOS.
#
#   curl -fsSL https://raw.githubusercontent.com/perfectra1n/claude-code-sync/main/install.sh | sh
#
# Pass options after `sh -s --`, e.g. `... | sh -s -- --version v0.3.3 --dir /usr/local/bin`.
# Every option also has an environment variable, which the flag overrides.
#
#   --version <tag>    CCS_VERSION      Release to install (default: latest)
#   --dir <path>       CCS_INSTALL_DIR  Install directory (default: ~/.local/bin)
#   --libc gnu|musl    CCS_LIBC         Linux build flavour (default: musl, a static
#                                       binary that runs on any distro incl. Alpine/NixOS)
#
# Re-running the installer upgrades an existing install in place.

set -eu

REPO="perfectra1n/claude-code-sync"
BIN="claude-code-sync"

say() { printf '%s\n' "$*"; }
err() { printf 'error: %s\n' "$*" >&2; exit 1; }

# Kept inline rather than read from $0: when piped into `sh`, $0 is the shell.
usage() {
    cat <<EOF
Usage: install.sh [--version <tag>] [--dir <path>] [--libc gnu|musl]

  --version <tag>    Release to install (default: latest)          [CCS_VERSION]
  --dir <path>       Install directory (default: ~/.local/bin)     [CCS_INSTALL_DIR]
  --libc gnu|musl    Linux build flavour (default: musl)           [CCS_LIBC]
EOF
}

# Fetch a URL to a file with whichever of curl/wget is available.
download() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --retry 3 -o "$2" "$1"
    elif command -v wget >/dev/null 2>&1; then
        wget -q -O "$2" "$1"
    else
        err "need curl or wget to download $BIN"
    fi
}

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d ' ' -f 1
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d ' ' -f 1
    else
        err "need sha256sum or shasum to verify the download"
    fi
}

detect_target() {
    os=$(uname -s)
    arch=$(uname -m)

    case "$arch" in
        x86_64 | amd64) arch=x86_64 ;;
        aarch64 | arm64) arch=aarch64 ;;
        *) err "unsupported CPU architecture: $arch (prebuilt: x86_64, aarch64). Try: cargo install claude-code-sync" ;;
    esac

    case "$os" in
        Linux)
            case "$libc" in
                musl) target="linux-$arch-musl" ;;
                gnu) target="linux-$arch" ;;
                *) err "--libc must be gnu or musl, got: $libc" ;;
            esac
            ;;
        Darwin)
            # A shell running under Rosetta reports x86_64 on Apple Silicon;
            # prefer the native build when the hardware supports it.
            if [ "$arch" = x86_64 ] && [ "$(sysctl -n hw.optional.arm64 2>/dev/null || echo 0)" = 1 ]; then
                arch=aarch64
            fi
            target="macos-$arch"
            ;;
        MINGW* | MSYS* | CYGWIN* | Windows_NT)
            err "on Windows use the PowerShell installer: irm https://raw.githubusercontent.com/$REPO/main/install.ps1 | iex"
            ;;
        *) err "unsupported OS: $os. Try: cargo install claude-code-sync" ;;
    esac
}

main() {
    version="${CCS_VERSION:-latest}"
    install_dir="${CCS_INSTALL_DIR:-${HOME:?HOME is not set}/.local/bin}"
    libc="${CCS_LIBC:-musl}"

    while [ $# -gt 0 ]; do
        case "$1" in
            --version) [ $# -ge 2 ] || err "--version needs a value"; version="$2"; shift 2 ;;
            --version=*) version="${1#*=}"; shift ;;
            --dir) [ $# -ge 2 ] || err "--dir needs a value"; install_dir="$2"; shift 2 ;;
            --dir=*) install_dir="${1#*=}"; shift ;;
            --libc) [ $# -ge 2 ] || err "--libc needs a value"; libc="$2"; shift 2 ;;
            --libc=*) libc="${1#*=}"; shift ;;
            -h | --help) usage; exit 0 ;;
            *) err "unknown option: $1 (see --help)" ;;
        esac
    done

    detect_target
    asset="$BIN-$target.tar.gz"

    if [ "$version" = latest ]; then
        base="https://github.com/$REPO/releases/latest/download"
    else
        case "$version" in v*) ;; *) version="v$version" ;; esac
        base="https://github.com/$REPO/releases/download/$version"
    fi

    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT INT TERM

    say "Downloading $asset ($version)..."
    download "$base/$asset" "$tmp/$asset" ||
        err "download failed: $base/$asset (does release $version have a $target build?)"
    download "$base/$asset.sha256" "$tmp/$asset.sha256" ||
        err "checksum download failed: $base/$asset.sha256"

    expected=$(cut -d ' ' -f 1 <"$tmp/$asset.sha256")
    actual=$(sha256_of "$tmp/$asset")
    [ "$expected" = "$actual" ] || err "checksum mismatch for $asset (expected $expected, got $actual)"

    tar -xzf "$tmp/$asset" -C "$tmp"
    [ -f "$tmp/$BIN" ] || err "archive did not contain $BIN"

    mkdir -p "$install_dir" 2>/dev/null ||
        err "cannot create $install_dir (re-run with --dir, or with sudo for a system path)"
    [ -w "$install_dir" ] ||
        err "$install_dir is not writable (re-run with --dir, or with sudo for a system path)"

    # Stage next to the destination then rename, so a running copy is never
    # left half-written.
    cp "$tmp/$BIN" "$install_dir/.$BIN.new"
    chmod 755 "$install_dir/.$BIN.new"
    mv -f "$install_dir/.$BIN.new" "$install_dir/$BIN"

    say "Installed $("$install_dir/$BIN" --version) to $install_dir/$BIN"

    case ":$PATH:" in
        *":$install_dir:"*) ;;
        *)
            say ""
            say "Note: $install_dir is not on your PATH. Add it with:"
            say "  export PATH=\"$install_dir:\$PATH\""
            ;;
    esac
}

main "$@"
