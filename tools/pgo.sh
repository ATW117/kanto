#!/usr/bin/env bash
# Profile-guided optimisation for the native simulator.
#
#   tools/pgo.sh [profile-dir]          # train, then print the RUSTFLAGS to build with
#
# 1. builds the bench example instrumented (-Cprofile-generate),
# 2. plays native games on three team sources (randbats, the gen1ou pool, random legal sets)
#    with seeds the benchmarks do not use (updates + legal choices, no-op event sink),
# 3. merges the raw profiles with the toolchain's llvm-profdata (`rustup component add
#    llvm-tools`) into <profile-dir>/merged.profdata.
#
# Then build anything with
#   RUSTFLAGS="-Cprofile-use=<profile-dir>/merged.profdata -Cllvm-args=-no-pgo-warn-mismatch" \
#     cargo build --release --target <host triple> ...
# (`--target` keeps RUSTFLAGS away from build scripts and proc macros.) PGO changes code layout
# and inlining only, never semantics: the parity suite runs unchanged on a PGO build.
set -euo pipefail
cd "$(dirname "$0")/.."
DIR=$(realpath -m "${1:-target/pgo}")
HOST=$(rustc -vV | sed -n 's/^host: //p')
SYSROOT=$(rustc --print sysroot)
PROFDATA="$SYSROOT/lib/rustlib/$HOST/bin/llvm-profdata"
[ -x "$PROFDATA" ] || { echo "llvm-profdata not found: rustup component add llvm-tools" >&2; exit 1; }
JOBS=${JOBS:-4}
rm -rf "$DIR/raw" && mkdir -p "$DIR/raw"
RUSTFLAGS="-Cprofile-generate=$DIR/raw" CARGO_TARGET_DIR="$DIR/target-gen" \
  cargo build -j "$JOBS" --release --target "$HOST" -p kanto-backend --example bench
BIN="$DIR/target-gen/$HOST/release/examples/bench"
"$BIN" 20000 randbats 90001 native
"$BIN" 20000 gen1ou 90002 native
"$BIN" 10000 random 90003 native
"$PROFDATA" merge -o "$DIR/merged.profdata" "$DIR"/raw/*.profraw
echo "profile: $DIR/merged.profdata"
echo "RUSTFLAGS=\"-Cprofile-use=$DIR/merged.profdata -Cllvm-args=-no-pgo-warn-mismatch\" cargo build --release --target $HOST ..."
