#!/bin/bash
set -e

echo "=========================================="
echo "  AION 离线安装脚本"
echo "=========================================="
echo ""

if [ "$EUID" -ne 0 ]; then
    echo "请使用 sudo 运行此脚本: sudo bash install.sh"
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# 1. 安装 Rust
echo "[1/4] 安装 Rust 工具链..."
if command -v rustc &> /dev/null; then
    echo "✓ Rust 已安装: $(rustc --version)"
else
    RUST_TAR=$(ls "$SCRIPT_DIR"/rust-*.tar.gz 2>/dev/null | head -1)
    if [ -n "$RUST_TAR" ]; then
        echo "使用本地 Rust 安装包: $(basename $RUST_TAR)"
        cd /tmp
        tar xzf "$RUST_TAR"
        RUST_DIR=$(ls -d rust-* 2>/dev/null | head -1)
        cd "$RUST_DIR"
        ./install.sh --prefix=/opt/rust
        cd "$SCRIPT_DIR"
        rm -rf "/tmp/$RUST_DIR"
        
        # 配置环境变量
        cat > /etc/profile.d/rust.sh << 'PROFILE'
export PATH="/opt/rust/bin:$PATH"
PROFILE
        source /etc/profile.d/rust.sh
        echo "✓ Rust 安装完成"
    else
        echo "✗ 未找到 Rust 安装包"
        exit 1
    fi
fi

# 2. 配置本地依赖
echo "[2/4] 配置本地依赖源..."
mkdir -p "$SCRIPT_DIR/.cargo"
cat > "$SCRIPT_DIR/.cargo/config.toml" << 'CARGO'
[source.crates-io]
replace-with = "vendored-sources"
[source.vendored-sources]
directory = "vendor"
CARGO
echo "✓ 配置完成"

# 3. 检查 vendor
echo "[3/4] 检查依赖包..."
if [ -d "$SCRIPT_DIR/vendor" ]; then
    echo "✓ 找到 vendor 目录 ($(du -sh "$SCRIPT_DIR/vendor" | cut -f1))"
else
    echo "✗ 未找到 vendor 目录"
    exit 1
fi

# 4. 编译
echo "[4/4] 编译 AION..."
export PATH="/opt/rust/bin:$HOME/.cargo/bin:$PATH"
cd "$SCRIPT_DIR"
cargo build --release -p aion 2>&1 | tail -20

echo ""
echo "=========================================="
echo "  ✓ 安装完成!"
echo "=========================================="
echo ""
echo "运行演示: cargo run -p aion -- demo"
echo "交互终端: cargo run -p aion -- repl"
echo "执行任务: cargo run -p aion -- run --agent assistant --kind chat --input '你好'"
