#!/usr/bin/env bash
# AION 一键安装（预编译 release 包 · 工作流 C）—— 免 Rust / 免编译。
#
# 两种用法等价：
#   curl -fsSL https://github.com/JayCRL/AION/releases/latest/download/install.sh | bash
#   # 或下载对应架构的 aion-<arch>-linux.tar.gz，解包后 ./install.sh
#
# 装到哪里（均可用环境变量覆盖）：
#   二进制     ~/.local/bin/aion                     AION_INSTALL_DIR
#   工作目录   ~/.config/aion                        AION_WORK_DIR  （aion.json / aion.providers.json 落这）
#   用户服务   ~/.config/systemd/user/aion.service   开机自启；无 systemd 则 nohup 前台
#   端口       8080                                  AION_PORT
#
# 装完开 http://localhost:8080 —— 首次在网页里配 LLM 供应商（存进 ~/.config/aion，
# 不再需要手写任何文件）。桌面能力（放视频/看图/开文档）需要登录图形会话。
set -euo pipefail

REPO="${AION_REPO:-JayCRL/AION}"
VER="${AION_VERSION:-latest}"
INSTALL_DIR="${AION_INSTALL_DIR:-$HOME/.local/bin}"
WORK_DIR="${AION_WORK_DIR:-$HOME/.config/aion}"
PORT="${AION_PORT:-8080}"
BASE="https://github.com/$REPO/releases/$VER/download"

RED=$'\033[0;31m'; GREEN=$'\033[0;32m'; YELLOW=$'\033[1;33m'; NC=$'\033[0m'
say(){ printf "${GREEN}✓${NC} %s\n" "$*"; }
warn(){ printf "${YELLOW}⚠${NC} %s\n" "$*" >&2; }
die(){ printf "${RED}✗${NC} %s\n" "$*" >&2; exit 1; }

detect_arch(){
  case "$(uname -m)" in
    x86_64|amd64) echo x86_64 ;;
    aarch64|arm64) echo aarch64 ;;
    *) die "不支持的架构 $(uname -m)（当前只出 x86_64 / aarch64 Linux 预编译包）" ;;
  esac
}
ARCH="$(detect_arch)"
ASSET="aion-${ARCH}-linux.tar.gz"

echo "=============================================="
echo "  AION 一键安装 · ${ARCH} · v${VER}"
echo "=============================================="

# 判断是否已在解包好的 release 目录里（tar.gz 内含同名 aion/install.sh）：
# 是 → 直接用旁边文件；否（多为 curl | bash）→ 先拉对应架构 tar.gz 下来再解包。
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" 2>/dev/null && pwd)"
TMP=""
if [ -n "${BASH_SOURCE[0]:-}" ] && [ -x "$SCRIPT_DIR/aion" ]; then
  SRC_DIR="$SCRIPT_DIR"
  say "使用已解包的本地文件（$SRC_DIR/aion）"
else
  TMP="$(mktemp -d)"
  trap 'rm -rf "$TMP"' EXIT
  say "下载 $REPO@$VER 的 $ASSET …"
  curl -fL --retry 3 "$BASE/$ASSET" -o "$TMP/$ASSET"
  tar xzf "$TMP/$ASSET" -C "$TMP"
  SRC_DIR="$TMP"
fi

# 1) 二进制 → ~/.local/bin（免 root）
mkdir -p "$INSTALL_DIR"
say "安装二进制 → $INSTALL_DIR/aion"
install -m 0755 "$SRC_DIR/aion" "$INSTALL_DIR/aion"

# 2) 工作目录 + 首次写 aion.json 模板（已存在则保留，用户改过的别覆盖）
mkdir -p "$WORK_DIR"
if [ ! -f "$WORK_DIR/aion.json" ]; then
  cp "$SRC_DIR/aion.json" "$WORK_DIR/aion.json"
  say "写入配置模板 → $WORK_DIR/aion.json"
else
  say "保留已有配置 $WORK_DIR/aion.json"
fi

# 3) systemd user unit（模板端口固定 8080；AION_PORT 覆盖时替换）
UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
mkdir -p "$UNIT_DIR"
UNIT="$UNIT_DIR/aion.service"
sed "s|--port 8080|--port $PORT|" "$SRC_DIR/aion.service" > "$UNIT"
say "写入用户服务 → $UNIT（端口 $PORT）"

start_systemd(){
  export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
  if systemctl --user daemon-reload && systemctl --user enable --now aion.service; then
    sleep 2
    if curl -fsS -o /dev/null "http://127.0.0.1:$PORT/api/health"; then
      say "服务已启动并通过健康检查"
      return 0
    fi
    warn "服务已拉起但健康检查未通过，稍后看日志：journalctl --user -u aion -f"
    return 0
  fi
  warn "连不上 user systemd bus（可能没在图形/登录会话里）"
  return 1
}
start_nohup(){
  say "回退：nohup 前台启动（本次会话内运行，重启不自动拉起）"
  nohup env HOME="$HOME" "$INSTALL_DIR/aion" web --port "$PORT" \
    >> "$WORK_DIR/aion-web.log" 2>&1 < /dev/null &
  sleep 2
}

if [ "$(ps -p 1 -o comm= 2>/dev/null)" = "systemd" ] && command -v systemctl >/dev/null 2>&1; then
  if ! start_systemd; then
    # 非交互 session 常无 DBUS：提示开启 linger 后重试，或先 nohup 顶着
    warn "建议以 root 执行：loginctl enable-linger $(id -un)  （开启后用户服务开机自启）"
    start_nohup
  fi
else
  warn "未检测到 systemd，退回 nohup 前台启动"
  start_nohup
fi

# 4) 收尾提示
case ":$PATH:" in
  *":$INSTALL_DIR:"*) : ;;
  *) warn "把 $INSTALL_DIR 加进 PATH：export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
esac

echo
echo "=============================================="
say "安装完成！浏览器打开  http://localhost:$PORT"
echo "首次使用：网页里 Settings → LLM → 「添加供应商」接一个真实模型"
echo "查看日志：journalctl --user -u aion -f"
echo "=============================================="
