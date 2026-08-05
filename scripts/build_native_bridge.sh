#!/bin/sh
set -eu

TARGET_TRIPLE=${1:-aarch64-unknown-linux-ohos}
PROFILE=${2:-release}
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
PROJECT_ROOT=$(CDPATH= cd -- "$(dirname -- "$SCRIPT_DIR")" && pwd)
NATIVE_CORE_DIR="$PROJECT_ROOT/native_rust_core"
BUILD_ROOT=${RUSTDESK_HARMONY_BUILD_DIR:-"$PROJECT_ROOT/_build"}
CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-"$BUILD_ROOT/native_rust_core/target"}
OUTPUT_DIR="$CARGO_TARGET_DIR/harmony"

. "$SCRIPT_DIR/_ohos-sdk-env.sh"

case "$TARGET_TRIPLE" in
  aarch64-unknown-linux-ohos)
    CLANG_TARGET=aarch64-linux-ohos
    SYSROOT_INCLUDE_DIR=aarch64-linux-ohos
    LIB_DIR=arm64
    ;;
  x86_64-unknown-linux-ohos)
    CLANG_TARGET=x86_64-linux-ohos
    SYSROOT_INCLUDE_DIR=x86_64-linux-ohos
    LIB_DIR=x86_64
    ;;
  *)
    echo "Unsupported target triple: $TARGET_TRIPLE" >&2
    exit 1
    ;;
esac

LINKER_SCRIPT="$SCRIPT_DIR/$TARGET_TRIPLE-clang.sh"
CXX_SCRIPT="$SCRIPT_DIR/$TARGET_TRIPLE-clang++.sh"
AR_SCRIPT="$SCRIPT_DIR/ohos-llvm-ar.sh"

for f in "$LINKER_SCRIPT" "$CXX_SCRIPT" "$AR_SCRIPT"; do
  [ -x "$f" ] || { echo "Missing or not executable: $f" >&2; exit 1; }
done

CARGO_BIN=$(command -v cargo || true)
[ -n "$CARGO_BIN" ] || CARGO_BIN="$HOME/.cargo/bin/cargo"
[ -x "$CARGO_BIN" ] || { echo "cargo was not found" >&2; exit 1; }

TARGET_KEY=$(printf '%s' "$TARGET_TRIPLE" | tr '[:lower:]-' '[:upper:]_')
TARGET_KEY_CC=$(printf '%s' "$TARGET_TRIPLE" | tr '[:upper:]' '[:lower:]' | sed 's/[-.]/_/g')
VCPKG_ROOT=${VCPKG_ROOT:-"$BUILD_ROOT/vcpkg"}
VCPKG_INSTALLED_ROOT=${VCPKG_INSTALLED_ROOT:-"$VCPKG_ROOT/installed"}
[ -d "$VCPKG_INSTALLED_ROOT" ] || { echo "vcpkg installed root was not found: $VCPKG_INSTALLED_ROOT" >&2; exit 1; }

COMMON_CFLAGS="--target=$CLANG_TARGET --sysroot=$SYSROOT -D__MUSL__ -fPIC"
BINDGEN_ARGS="--target=$CLANG_TARGET --sysroot=$SYSROOT -I$SYSROOT/usr/include/$SYSROOT_INCLUDE_DIR -I$SYSROOT/usr/include -D__MUSL__"

export "CARGO_TARGET_${TARGET_KEY}_LINKER=$LINKER_SCRIPT"
export "CARGO_TARGET_${TARGET_KEY}_AR=$AR_SCRIPT"
export "CC_${TARGET_KEY}=$LINKER_SCRIPT"
export "CXX_${TARGET_KEY}=$CXX_SCRIPT"
export "AR_${TARGET_KEY}=$AR_SCRIPT"
export "CC_${TARGET_KEY_CC}=$LINKER_SCRIPT"
export "CXX_${TARGET_KEY_CC}=$CXX_SCRIPT"
export "AR_${TARGET_KEY_CC}=$AR_SCRIPT"
export "CFLAGS_${TARGET_KEY_CC}=$COMMON_CFLAGS"
export "CXXFLAGS_${TARGET_KEY_CC}=$COMMON_CFLAGS"
export "LD_${TARGET_KEY_CC}=$LLVM_BIN/ld.lld"
export "NM_${TARGET_KEY_CC}=$LLVM_BIN/llvm-nm"
export "RANLIB_${TARGET_KEY_CC}=$LLVM_BIN/llvm-ranlib"
export "BINDGEN_EXTRA_CLANG_ARGS_${TARGET_KEY_CC}=$BINDGEN_ARGS"
export BINDGEN_EXTRA_CLANG_ARGS="$BINDGEN_ARGS"
export TARGET_CC="$LINKER_SCRIPT"
export TARGET_CXX="$CXX_SCRIPT"
export TARGET_AR="$AR_SCRIPT"
export LD="$LLVM_BIN/ld.lld"
export NM="$LLVM_BIN/llvm-nm"
export RANLIB="$LLVM_BIN/llvm-ranlib"
export VCPKG_ROOT VCPKG_INSTALLED_ROOT CARGO_TARGET_DIR
export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_SYSROOT_DIR="$SYSROOT"
export RUSTFLAGS="-C link-arg=--target=$CLANG_TARGET -C link-arg=-fuse-ld=lld"
export LIBCLANG_PATH="$LLVM_BIN/../lib"
export PATH="$LLVM_BIN:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"

# 设置 libsodium 预编译库路径（关键：使用不带后缀的 SODIUM_LIB_DIR）
TARGET_SODIUM_LIB_DIR=${SODIUM_LIB_DIR:-"$BUILD_ROOT/build/libsodium/$TARGET_TRIPLE/lib"}
if [ -d "$TARGET_SODIUM_LIB_DIR" ]; then
  export SODIUM_LIB_DIR="$TARGET_SODIUM_LIB_DIR"
  echo "Using precompiled libsodium from: $SODIUM_LIB_DIR"
else
  echo "WARNING: Precompiled libsodium not found at $TARGET_SODIUM_LIB_DIR"
  echo "Will attempt to build from source..."
fi
# 注意：不清除 SODIUM_LIB_DIR，让 cargo build 能访问到

# 清除旧的 libsodium-sys 构建缓存
rm -rf "$CARGO_TARGET_DIR/release/build"/libsodium-sys-* "$CARGO_TARGET_DIR/$TARGET_TRIPLE/$PROFILE/build"/libsodium-sys-* 2>/dev/null || true
rm -f "$CARGO_TARGET_DIR/release/deps"/liblibsodium_sys-*.rlib "$CARGO_TARGET_DIR/$TARGET_TRIPLE/$PROFILE/deps"/liblibsodium_sys-*.rlib 2>/dev/null || true

mkdir -p "$CARGO_TARGET_DIR"
cd "$NATIVE_CORE_DIR"
env "CC_$TARGET_TRIPLE=$LINKER_SCRIPT" "CXX_$TARGET_TRIPLE=$CXX_SCRIPT" "AR_$TARGET_TRIPLE=$AR_SCRIPT" "$CARGO_BIN" build --profile "$PROFILE" --target "$TARGET_TRIPLE"

ARTIFACT_DIR="$CARGO_TARGET_DIR/$TARGET_TRIPLE/$PROFILE"
SOURCE_LIB=""
[ -f "$ARTIFACT_DIR/librustdesk_harmony_bridge.a" ] && SOURCE_LIB="$ARTIFACT_DIR/librustdesk_harmony_bridge.a"
[ -z "$SOURCE_LIB" ] && [ -f "$ARTIFACT_DIR/rustdesk_harmony_bridge.a" ] && SOURCE_LIB="$ARTIFACT_DIR/rustdesk_harmony_bridge.a"
[ -n "$SOURCE_LIB" ] || { echo "Native bridge static library was not found in $ARTIFACT_DIR" >&2; exit 1; }

mkdir -p "$OUTPUT_DIR" "$PROJECT_ROOT/entry/src/main/libs/$LIB_DIR"
cp -f "$SOURCE_LIB" "$OUTPUT_DIR/librustdesk_harmony_bridge.a"
cp -f "$SOURCE_LIB" "$PROJECT_ROOT/entry/src/main/libs/$LIB_DIR/librustdesk_core.a"
printf 'Native bridge artifact copied to %s\n' "$OUTPUT_DIR/librustdesk_harmony_bridge.a"
printf 'Harmony package lib copied to %s\n' "$PROJECT_ROOT/entry/src/main/libs/$LIB_DIR/librustdesk_core.a"
