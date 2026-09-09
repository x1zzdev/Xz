#!/usr/bin/env bash
# Sets up the portable LLVM 17 toolchain used by xz-cli's LLVM backend.
#
# The LLVM backend (Phase 4) links against a home-directory, root-free install
# of LLVM 17 (Ubuntu packages extracted without installing). This script
# verifies the install and prints the environment variables the build needs.
# See docs/13-codegen.md (Environment) for the full rationale.
#
# Usage:
#   scripts/setup-llvm.sh           # check the install and print the env block
#   scripts/setup-llvm.sh --emit    # append the [env] block to .cargo/config.toml
#
# The variables are normally pinned already in xz-cli/.cargo/config.toml, so a
# plain `cargo build` in xz-cli/ works without this script.

set -euo pipefail

LLVM_ROOT="${XZ_LLVM_ROOT:-$HOME/.local/share/xz-llvm17}"
LLVM_PREFIX="$LLVM_ROOT/debroot/usr/lib/llvm-17"
LIB_DIR="$LLVM_ROOT/debroot/usr/lib/x86_64-linux-gnu"

if [ ! -x "$LLVM_PREFIX/bin/llvm-config" ]; then
    echo "error: LLVM 17 not found at $LLVM_PREFIX" >&2
    echo "Recreate it by extracting the Ubuntu llvm-17 debs into $LLVM_ROOT/debroot," >&2
    echo "or set XZ_LLVM_ROOT to the extracted debroot location." >&2
    exit 1
fi

version="$("$LLVM_PREFIX/bin/llvm-config" --version)"
echo "portable LLVM: $version at $LLVM_PREFIX"

if [ ! -e "$LIB_DIR/libLLVM-17.so.1" ]; then
    echo "error: shared libLLVM-17.so.1 not found in $LIB_DIR" >&2
    exit 1
fi

if [ "${1:-}" = "--emit" ]; then
    config="$(dirname "$0")/../.cargo/config.toml"
    echo "appending LLVM env block to $config"
    cat >> "$config" <<EOF

[env]
LLVM_SYS_170_PREFIX = { value = "$LLVM_PREFIX", force = true }
LIBRARY_PATH = { value = "$LIB_DIR", force = true }
EOF
fi

echo ""
echo "export LLVM_SYS_170_PREFIX=\"$LLVM_PREFIX\""
echo "export LIBRARY_PATH=\"$LIB_DIR\""
