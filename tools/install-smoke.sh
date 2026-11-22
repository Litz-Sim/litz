#!/bin/sh
# install-smoke.sh — install the *published* litz-cli from crates.io and run
# the repository's blinky example through the installed binary.
#
# This is a release-verification check, not a workspace test. Unit tests and
# `cargo run -p litz-cli` build against this checkout's path dependencies, so
# they can pass even when the crate graph that was actually published is
# missing a file, a feature or a template. Installing the crate by name and
# version from the registry exercises the exact tarball a user gets.
#
# It needs network access: `cargo install` downloads litz-cli and every crate
# in its published dependency graph from the registry (or from --registry).
# The example firmware is committed, so running the example itself is offline.
#
# The install lives under <repo>/build/install-smoke (never /tmp) and is
# removed when the script exits unless --keep is given. build/ is gitignored.

set -eu

# Resolve the repository root from this script's own path so the check works
# from any working directory.
script=$0
case $script in
    /*) ;;
    *) script=$PWD/$script ;;
esac
root=$(CDPATH= cd -- "$(dirname -- "$script")/.." && pwd)

install_root=$root/build/install-smoke
keep=0
version=
registry=

usage() {
    cat <<EOF
Usage: tools/install-smoke.sh [options]

Install the published litz-cli crate and run examples/blinky through it.

Options:
  --version <ver>    Install this litz-cli version (default: latest release)
  --registry <name>  Pass --registry <name> to cargo install (default: crates.io)
  --keep             Keep the install root at build/install-smoke on exit
  -h, --help         Show this help and exit

Requirements: cargo on PATH and network access to the registry. The example
firmware is committed, so running the example needs no downloads.

Exit status: 0 when the install and the blinky test both pass, otherwise the
exit code of the failing step (1 for a failed test, 2 for bad usage).
EOF
}

die_usage() {
    printf 'install-smoke: %s\n' "$1" >&2
    usage >&2
    exit 2
}

while [ "$#" -gt 0 ]; do
    case $1 in
        --version)
            [ "$#" -ge 2 ] || die_usage "--version needs a value"
            version=$2
            shift 2
            ;;
        --version=*)
            version=${1#--version=}
            [ -n "$version" ] || die_usage "--version needs a value"
            shift
            ;;
        --registry)
            [ "$#" -ge 2 ] || die_usage "--registry needs a value"
            registry=$2
            shift 2
            ;;
        --registry=*)
            registry=${1#--registry=}
            [ -n "$registry" ] || die_usage "--registry needs a value"
            shift
            ;;
        --keep)
            keep=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            die_usage "unknown argument: $1"
            ;;
    esac
done

cleanup() {
    if [ "$keep" -eq 0 ] && [ -n "$install_root" ] && [ "$install_root" != / ]; then
        rm -rf -- "$install_root"
    fi
}

trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Start from a clean root every run so a stale binary can never be mistaken
# for the freshly installed one.
if [ "$install_root" = / ] || [ -z "$install_root" ]; then
    printf 'install-smoke: refusing to use %s as the install root\n' "$install_root" >&2
    exit 1
fi
rm -rf -- "$install_root"
mkdir -p -- "$root/build"

# Build the cargo install invocation. With no --version, cargo picks the newest
# published release. --locked makes it honour the published Cargo.lock.
set -- install litz-cli --locked --root "$install_root"
if [ -n "$version" ]; then
    set -- "$@" --version "$version"
fi
if [ -n "$registry" ]; then
    set -- "$@" --registry "$registry"
fi

printf 'install-smoke: installing published litz-cli%s\n' \
    "${version:+ $version}" >&2
if cargo "$@"; then
    :
else
    rc=$?
    printf 'install-smoke: FAIL — cargo install litz-cli failed (exit %s)\n' "$rc" >&2
    exit "$rc"
fi

litz=$install_root/bin/litz
if [ ! -x "$litz" ]; then
    printf 'install-smoke: FAIL — installed binary not found at %s\n' "$litz" >&2
    exit 1
fi

# `litz --version` prints "litz <version>" and exits 0 (see crates/litz-cli).
printf 'install-smoke: installed binary: ' >&2
if "$litz" --version; then
    :
else
    rc=$?
    printf 'install-smoke: FAIL — litz --version failed (exit %s)\n' "$rc" >&2
    exit "$rc"
fi

# The binary carries the builtin catalog through its litz-catalog dependency.
# A packaged lockfile can pin a catalog older than the one this tree ships, and
# nothing shows it until a diagram uses an attribute the old copy lacks. The
# bench supply's current_limit shipped after the first catalog release, so it
# is the attribute that catches that drift.
if "$litz" catalog --json | grep -q current_limit; then
    :
else
    printf 'install-smoke: FAIL — the installed catalog has no current_limit attribute\n' >&2
    exit 1
fi

# Run the example from the repository root so the relative project path and
# the committed firmware resolve. `litz test` exits 0 on pass, 1 on failure.
printf 'install-smoke: running litz test examples/blinky\n' >&2
if (cd "$root" && "$litz" test examples/blinky); then
    printf 'install-smoke: PASS — published litz-cli installs and examples/blinky passes%s\n' \
        "${version:+ (version $version)}"
    if [ "$keep" -eq 1 ]; then
        printf 'install-smoke: kept install root at %s\n' "$install_root"
    fi
else
    rc=$?
    printf 'install-smoke: FAIL — litz test examples/blinky failed (exit %s)%s\n' \
        "$rc" "${version:+ (version $version)}" >&2
    exit "$rc"
fi
