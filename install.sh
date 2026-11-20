#!/bin/sh
# Installs the hum release binary for this machine.
#
#   curl -fsSL https://github.com/danczw/hum/releases/latest/download/install.sh | sh
#
# HUM_VERSION picks a release (e.g. 0.2.0, default latest), HUM_INSTALL_DIR
# the target directory (default ~/.local/bin).
#
# Everything runs from main on the last line, so a download cut short by
# `curl | sh` runs nothing.
set -eu

err() {
    echo "install.sh: $*" >&2
    exit 1
}

main() {
    repo=danczw/hum
    dir=${HUM_INSTALL_DIR:-$HOME/.local/bin}

    [ "$(uname -s)" = Linux ] || err "hum runs on Linux only"

    # i686 builds need SSE2, so i386 to i586 get no match.
    case $(uname -m) in
        x86_64 | amd64) target=x86_64-unknown-linux-musl ;;
        i686) target=i686-unknown-linux-musl ;;
        aarch64 | arm64) target=aarch64-unknown-linux-musl ;;
        armv7* | armv8l) target=armv7-unknown-linux-musleabihf ;;
        armv6*) target=arm-unknown-linux-musleabihf ;;
        riscv64) target=riscv64gc-unknown-linux-musl ;;
        ppc64le) target=powerpc64le-unknown-linux-musl ;;
        loongarch64) target=loongarch64-unknown-linux-musl ;;
        *) err "no release for $(uname -m)" ;;
    esac

    if [ -n "${HUM_VERSION:-}" ]; then
        base=https://github.com/$repo/releases/download/v${HUM_VERSION#v}
    else
        base=https://github.com/$repo/releases/latest/download
    fi

    if command -v curl >/dev/null; then
        fetch() { curl -fsSL -o "$2" "$1"; }
    elif command -v wget >/dev/null; then
        fetch() { wget -qO "$2" "$1"; }
    else
        err "needs curl or wget"
    fi

    if command -v sha256sum >/dev/null; then
        sha256() { sha256sum "$1"; }
    elif command -v shasum >/dev/null; then
        sha256() { shasum -a 256 "$1"; }
    else
        err "needs sha256sum or shasum"
    fi

    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    archive=hum-$target.tar.gz

    fetch "$base/$archive" "$tmp/$archive" || err "download failed: $base/$archive"
    fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" || err "download failed: $base/SHA256SUMS"

    want=$(awk -v f="$archive" '$2 == f { print $1 }' "$tmp/SHA256SUMS")
    got=$(sha256 "$tmp/$archive" | cut -d' ' -f1)
    [ -n "$want" ] || err "$archive missing from SHA256SUMS"
    [ "$want" = "$got" ] || err "checksum mismatch for $archive"

    tar -xzf "$tmp/$archive" -C "$tmp"
    mkdir -p "$dir"
    cp "$tmp/hum" "$dir/hum.tmp"
    chmod 755 "$dir/hum.tmp"
    mv "$dir/hum.tmp" "$dir/hum"

    echo "installed $("$dir/hum" --version) to $dir/hum"
    case :$PATH: in
        *:"$dir":*) ;;
        *) echo "note: $dir is not on PATH" ;;
    esac
}

main "$@"
