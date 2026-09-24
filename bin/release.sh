#!/usr/bin/env sh
# Build claude-code-sync for every platform from one Linux or macOS host, into dist/.
# Prerequisites: the rustup targets below, `cargo install cargo-zigbuild`, zig and
# python3 on PATH.
#
# Releases come from CI (.github/workflows/release.yml builds each tag on its own
# runner and attaches the binaries to the GitHub release); this is for building all
# three locally without waiting for a tag.
#
# The `dist` profile, not `release`: built for size, and separate so `cargo install`
# and the published release artifacts keep the crate's own settings.
set -eu

repo="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
cd "$repo"

# Rust's Windows std imports WaitOnAddress and friends from `synchronization`, an API-set
# forwarder whose import library zig's mingw does not ship. zig does ship the API set's
# own .def, so the import library is generated from it under that name.
windows_import_libs() {
    libs="$repo/target/windows-import-libs"
    zig_lib_dir="$(zig env | sed -n 's/^[ .]*lib_dir[ ]*[:=][ ]*"\([^"]*\)".*/\1/p')"
    def="$zig_lib_dir/libc/mingw/lib-common/api-ms-win-core-synch-l1-2-0.def"
    [ -f "$def" ] || {
        echo "release.sh: $def not found; is zig on PATH?" >&2
        exit 1
    }
    mkdir -p "$libs"
    zig dlltool -m i386:x86-64 -d "$def" -l "$libs/libsynchronization.a" >&2
    echo "$libs"
}

build() { # <rust target> [extra RUSTFLAGS]
    printf '%-32s' "$1"
    RUSTFLAGS="${2:-}" cargo zigbuild --profile dist --quiet --target "$1"
    echo built
}

build x86_64-unknown-linux-musl
build x86_64-pc-windows-gnu "-L $(windows_import_libs)"
build aarch64-apple-darwin
build x86_64-apple-darwin

out="$repo/dist"
mkdir -p "$out"
cp target/x86_64-unknown-linux-musl/dist/claude-code-sync "$out/claude-code-sync-linux-x64"
cp target/x86_64-pc-windows-gnu/dist/claude-code-sync.exe "$out/claude-code-sync.exe"

# One universal Mach-O instead of two files. Apple's `lipo` does not exist on Linux, so
# the fat container is assembled directly; see bin/make-universal.py.
python3 "$repo/bin/make-universal.py" "$out/claude-code-sync-macos" \
    target/aarch64-apple-darwin/dist/claude-code-sync \
    target/x86_64-apple-darwin/dist/claude-code-sync

chmod +x "$out/claude-code-sync-linux-x64" "$out/claude-code-sync-macos" \
    "$out/claude-code-sync.exe"

echo
echo "Built into $out"
