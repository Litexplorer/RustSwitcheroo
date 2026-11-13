#!/usr/bin/env bash
# 在 Linux/macOS 上交叉编译 Windows x64 版本（gnu 目标，静态链接 CRT）。
#
# 用法：
#   bash scripts/build-windows.sh                 # 编译 + 打包
#   bash scripts/build-windows.sh --no-package    # 只编译，不动 dist/
#   bash scripts/build-windows.sh --help
#
# 产物：
#   target/x86_64-pc-windows-gnu/dist/switcheroo.exe
#   dist/switcheroo-win-x64/switcheroo.exe, README.txt
#   dist/switcheroo-win-x64.zip（没有 zip 时退化为 .tar.gz）
#
# 依赖：rustup + 目标 x86_64-pc-windows-gnu、mingw-w64 的 windres/objdump（可选：zip）。
# 脚本可重复执行；任何一步失败都会打印中文原因并以非 0 退出。

set -euo pipefail

TARGET="x86_64-pc-windows-gnu"
PACKAGE="switcheroo-app"
BIN_NAME="switcheroo"
PKG_NAME="switcheroo-win-x64"

usage() {
    cat <<'EOF'
用法：bash scripts/build-windows.sh [选项]

选项：
  --no-package   只编译，不组装 dist/ 目录、不生成压缩包
  -h, --help     显示本帮助

说明：在 Linux/macOS 上交叉编译 Windows x64（gnu 目标）版本。
EOF
}

NO_PACKAGE=0
while [ $# -gt 0 ]; do
    case "$1" in
        --no-package) NO_PACKAGE=1 ;;
        -h|--help) usage; exit 0 ;;
        *)
            echo "❌ 未知参数：$1" >&2
            usage >&2
            exit 1
            ;;
    esac
    shift
done

# 仓库根目录 = 本脚本所在目录的上一级；全部路径都相对它推导，不写死绝对路径。
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

DIST_DIR="$REPO_ROOT/dist"
PKG_DIR="$DIST_DIR/$PKG_NAME"
EXE_PATH="$REPO_ROOT/target/$TARGET/dist/$BIN_NAME.exe"
WINDRES_BIN="${WINDRES:-x86_64-w64-mingw32-windres}"
OBJDUMP_BIN="x86_64-w64-mingw32-objdump"
TOTAL_STEPS=5

echo "==> 构建 Windows 版本（$TARGET，profile=dist）"
echo "    仓库根目录：$REPO_ROOT"
echo

# ---- [1/5] 确保 Rust target 已安装 ----
echo "[1/$TOTAL_STEPS] 检查 Rust target $TARGET ..."
if ! command -v rustup >/dev/null 2>&1; then
    echo "❌ 未找到 rustup，无法确认/安装 $TARGET。" >&2
    echo "   请安装 rustup 后执行：rustup target add $TARGET" >&2
    exit 1
fi
if rustup target list --installed 2>/dev/null | grep -Fxq "$TARGET"; then
    echo "    ✅ 已安装"
else
    echo "    ⏳ 未安装，执行 rustup target add $TARGET ..."
    if ! rustup target add "$TARGET"; then
        echo "❌ rustup target add $TARGET 失败。" >&2
        echo "   请检查网络/代理，或手动执行：rustup target add $TARGET" >&2
        exit 1
    fi
    echo "    ✅ 安装完成"
fi

# ---- [2/5] 检查资源编译器 ----
echo "[2/$TOTAL_STEPS] 检查 windres（用于把图标嵌入 exe）..."
if command -v "$WINDRES_BIN" >/dev/null 2>&1; then
    echo "    ✅ 找到：$(command -v "$WINDRES_BIN")"
else
    echo "    ⚠️  未找到 $WINDRES_BIN：图标将不会被嵌入（exe 使用默认图标），继续编译。"
    echo "       Debian/Ubuntu 可安装：apt install mingw-w64"
fi

# ---- [3/5] 编译 ----
echo "[3/$TOTAL_STEPS] 编译（cargo build --profile dist）..."
if ! cargo build --profile dist -p "$PACKAGE" --target "$TARGET"; then
    echo "❌ 编译失败，见上方 cargo 输出。" >&2
    exit 1
fi
if [ ! -f "$EXE_PATH" ]; then
    echo "❌ 编译结束但找不到产物：$EXE_PATH" >&2
    exit 1
fi
echo "    ✅ 产物：$EXE_PATH"

# ---- [4/5] 检查 .rsrc 资源节 ----
echo "[4/$TOTAL_STEPS] 检查 exe 是否带 .rsrc 资源节 ..."
if ! command -v "$OBJDUMP_BIN" >/dev/null 2>&1; then
    echo "    ⚠️  未找到 $OBJDUMP_BIN，跳过资源节检查。"
elif "$OBJDUMP_BIN" -h "$EXE_PATH" 2>/dev/null \
    | awk '$2 == ".rsrc" { found = 1 } END { exit !found }'; then
    echo "    ✅ 已找到 .rsrc 节：图标已嵌入"
else
    echo "    ⚠️  未找到 .rsrc 节：exe 将使用默认图标（不影响运行）"
fi

# ---- [5/5] 组装 + 打包 ----
if [ "$NO_PACKAGE" -eq 1 ]; then
    echo "[5/$TOTAL_STEPS] 跳过打包（--no-package）"
    echo
    echo "✅ 完成（未打包）。exe：$EXE_PATH"
    exit 0
fi

echo "[5/$TOTAL_STEPS] 组装 dist/$PKG_NAME ..."
rm -rf "$PKG_DIR"
mkdir -p "$PKG_DIR"
cp "$EXE_PATH" "$PKG_DIR/$BIN_NAME.exe"
if [ -f "$REPO_ROOT/README.md" ]; then
    cp "$REPO_ROOT/README.md" "$PKG_DIR/README.txt"
    echo "    已复制 README.md -> README.txt"
else
    echo "    ⚠️  未找到 README.md，跳过说明文件。"
fi
echo "    exe 体积：$(du -h "$PKG_DIR/$BIN_NAME.exe" | cut -f1)"

if command -v zip >/dev/null 2>&1; then
    ARCHIVE="$DIST_DIR/$PKG_NAME.zip"
    rm -f "$ARCHIVE"
    (cd "$DIST_DIR" && zip -qr "$PKG_NAME.zip" "$PKG_NAME")
    echo "    ✅ 压缩包：$ARCHIVE（$(du -h "$ARCHIVE" | cut -f1)）"
else
    ARCHIVE="$DIST_DIR/$PKG_NAME.tar.gz"
    rm -f "$ARCHIVE"
    tar -czf "$ARCHIVE" -C "$DIST_DIR" "$PKG_NAME"
    echo "    ⚠️  未找到 zip，已退化为 tar.gz：$ARCHIVE（$(du -h "$ARCHIVE" | cut -f1)）"
fi

echo
echo "✅ 完成。交付目录：$PKG_DIR"
echo "   压缩包：$ARCHIVE"
