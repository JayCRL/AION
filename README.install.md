# AION — 预编译一键安装

这是 AION（Agent OS）面向普通 Linux 用户的免编译安装包：**无需 Rust、无需源码**。
前端已整体内嵌进二进制，单文件即完整运行时。

## 系统要求

- Linux x86_64 或 aarch64（glibc）
- 免 root：全部装到用户目录 + systemd **用户**服务
- 想要 apt 依赖自动补装：可选配 `sudo -n` NOPASSWD（见下方「能力依赖」）

## 安装

```bash
curl -fsSL https://github.com/JayCRL/AION/releases/latest/download/install.sh | bash
```

或手动下载并解包后执行：

```bash
tar xzf aion-x86_64-linux.tar.gz   # 或 aion-aarch64-linux.tar.gz
./install.sh
```

装到：

| 路径 | 内容 |
|------|------|
| `~/.local/bin/aion` | 主程序二进制 |
| `~/.config/aion/` | 工作目录：`aion.json` 配置模板 + `aion.providers.json`（网页配的 LLM 供应商） |
| `~/.config/systemd/user/aion.service` | 用户服务（systemd 环境自动 `enable --now`） |

> 非交互 SSH 会话常没有 DBUS，user 服务可能拉不起。以 root 执行一次
> `loginctl enable-linger <你的用户名>` 即可让用户服务登录自启；在那之前脚本会回退 `nohup` 顶着。

## 开始使用

1. 浏览器打开 **http://localhost:8080**
2. 首次进入：**Settings ⚙️ → LLM →「添加供应商」**，接一个真实模型
   （预设了智谱 GLM / DeepSeek / OpenAI / 通义千问；或填任意 Anthropic / OpenAI 兼容端点）。
   之后无需手写任何配置文件。
3. 回到对话框输入一句话 —— AION 会按需调用工具在这台机器上干活。

## 能力与依赖

AION 把「放视频 / 看图 / 开文档 / 浏览网页」等做成了 **Capabilities**（设置页可见，可开关）：

- 缺外部软件时，能力广场会标红 ✗ 并给「安装 N 项依赖」/「一键补齐全部缺失」按钮。
- 用户级独立二进制（yt-dlp、moli 网页引擎）直接下载到 `~/.local/bin`，免 root。
- 系统包（mpv、feh、zathura、libreoffice…）走 `apt-get`，需要 root：
  两种方式——
  - 给 AION 配 NOPASSWD（`echo '<你的用户> ALL=(ALL) NOPASSWD: ALL' | sudo tee /etc/sudoers.d/aion`），之后「一键补齐」全自动；
  - 不配也行：点安装会提示你**手动执行**对应的 `sudo apt-get install …`。
- 模型调用某能力但依赖没齐时，会先弹「确认补装依赖」门，同意后自动装完并继续执行。

「本机软件档案」按钮会扫描已知软件全集，逐项标 ✓/✗ + 路径 + 版本（只探测，不改装）。

## 常用运维

```bash
# 看日志
journalctl --user -u aion -f

# 停止 / 启动 / 状态
systemctl --user stop aion      # 停止
systemctl --user start aion     # 启动
systemctl --user status aion    # 状态

# 开机自启（linger 已开则无需此步）
systemctl --user enable aion

# 升级（重跑安装脚本即可，会保留 ~/.config/aion 已有配置）
curl -fsSL https://github.com/JayCRL/AION/releases/latest/download/install.sh | bash

# 卸载
systemctl --user disable --now aion.service
rm -f ~/.local/bin/aion ~/.config/systemd/user/aion.service
rm -rf ~/.config/aion          # 注意：这会删掉已配的 LLM 供应商与本地数据
```

换端口：`AION_PORT=18080 bash install.sh`（会改写 unit 里的 `--port`）。

## 安全说明

AION 是**有本机执行能力**的 Agent OS。安装即代表你信任它在本用户权限内跑工具；
高风险命令（高危 shell、装系统包）都会弹确认框等你点「同意」才执行。

沙箱在无特权时自动降级：非 root 运行的 AION 无法建 cgroup v2 子组或隔离 namespace
（需要 root / systemd delegate 授权）。此时资源限制与进程隔离**尽力而为**，启动日志会
如实标 `cgroup ✗ · namespace ✗`，进程照常运行——不影响使用，只是进程不额外套资源墙。
要真隔离：以 root 配 `systemd Delegate` 或直接以 root 运行 AION。
