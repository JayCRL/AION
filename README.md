<div align="center">

<img src="assets/logo.png" alt="AION logo" width="280"/>

# AION

**Agent OS Runtime · AI 层替代应用层：用户只与 AI 对话，系统服务直连 Linux 内核**

基于 Linux 的 Agent「操作系统」运行时：Cordis-RS 核心 · 系统服务 · Linux 适配层

[![CI](https://github.com/JayCRL/AION/actions/workflows/ci.yml/badge.svg)](https://github.com/JayCRL/AION/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://www.rust-lang.org)

</div>

---

## 项目定位

**AION 不是"又一个通用 Agent 框架"，而是一台 Linux 设备上的专用 Agent 操作系统。**

- **核心主张**：用 AI 层替代应用层。用户不操作 App，只与 AI 对话，AI 通过系统服务直接满足需求。
- **与通用 Agent 框架（如 DeepSeek Harness / DSH）的分野**：

| | 通用 Agent 框架 | AION |
|---|---|---|
| 定位 | AI 作为应用 | AI 作为操作系统 |
| 工具来源 | 插件生态（可拼装） | 内核级系统服务（process/fs/net/terminal/media/sandbox/…） |
| 安全边界 | 审批弹窗（人在环） | namespace / cgroup v2 / seccomp / capability（机制在环） |
| 生命周期 | 会话制 | 常驻服务制（Scope / Fiber / Lifecycle） |
| 面向 | 会配置的开发者 | 设备的使用者 |

- **判定标准**（每次加功能前自问）：
  1. 这个功能服务于"对话替代 App"吗？不是 → 不做；
  2. 这个功能 DSH 加个插件/命令三分钟就能实现吗？是 → 不做（那不属于 AION 的护城河）；
  3. 它用到了只有内核层才能保证的东西（持久沙箱 / 多租户隔离 / 资源记账 / 事件驱动 / 常驻生命周期）吗？是 → 这才是 AION 该做的主线。

- **AION 不打算赢下的战场**：通用聊天 UI、LLM 供应商管理、跨平台桌面体验——这些成熟方案（DSH、cc-switch 等）已经做得更好，用户也感知不到差异。

---

AION 把 Agent 运行所需的「上下文、并发、隔离、权限、资源、模型」组织成一个类似操作系统的分层运行时：

- **应用层（Agents & Apps）**：Coder / Research / Assistant / Browser / Custom Agent，全部通过 Context 使用系统能力；
- **核心层（Cordis-RS Runtime）**：Context、Service Registry、Event Bus、Plugin System + Fiber、Effect、Scope、DI + Logger、Config、Loader、Lifecycle；
- **服务层（AION System Services）**：进程、文件、网络、终端、沙箱、存储、设备、模型八类服务，负责权限检查；
- **适配层（AION Linux Adapter）**：封装 namespace / cgroup v2 / seccomp / capability / mount 等 Linux 系统调用；
- **内核层（Linux Kernel）**：进程、文件系统、网络、资源控制、隔离、设备、安全（LSM/SELinux）。

## 安装（Linux · 免编译）

面向普通用户：**不需要 Rust、不需要编译**。预编译包在 [GitHub Releases](https://github.com/JayCRL/AION/releases)，
`x86_64` / `aarch64` 各一个自包含压缩包（Web 前端已内嵌进二进制，解包即完整运行时）。

```bash
# 一行安装：拉最新版 install.sh 并执行 → ~/.local/bin + systemd 用户服务
curl -fsSL https://github.com/JayCRL/AION/releases/latest/download/install.sh | bash
```

或手动下载：Releases 页取 `aion-x86_64-linux.tar.gz`（或 `aion-aarch64-linux.tar.gz`）→ 解包 → `./install.sh`。

装完浏览器打开 **http://localhost:8080**，首次在 ⚙️ Settings → LLM 添加一个真实模型即可对话干活。
全部装到用户目录、**免 root**；想让 AION 自动补装系统包（apt）可选配 `sudo -n` NOPASSWD。
细节（能力依赖、一键补齐、卸载、换端口、安全说明）见 **[README.install.md](README.install.md)**。

> 想自己编译、参与开发？跳到下方「从源码构建（开发者）」。

## 架构总览

![AION 架构总览](assets/architecture.png)

图中「② 能力主线 · Capability」与「③ 叶子工具 · Tool Provider」对应新版分层：Agent 不再直接面对
零散工具，而是通过能力（`web.view` / `file.view` / `media.view`）发起请求，由 Capability 的 Resolver
解析成具体叶子工具执行。图源为 `assets/architecture.html`，用 Moli 无头引擎
（`moli fetch --layout --dump screenshot_full`）截图再生成 `assets/architecture.png`。

## 调用流程

```
Agent 发起请求 → Cordis Context 获取 Service → AION Service 权限检查
              → Linux Adapter 执行系统调用 → Linux Kernel 执行操作
              → 资源返回（事件 / Effect）
```

一次 `aion demo` 会逐步打印上述六个阶段（包括权限拒绝的负例），并演示事件流与 Effect 清理。

## 从源码构建（开发者）

普通用户装预编译包即可（见上）；这里给想自己编译、改内核、跑 CI 的人。

```bash
# 构建并运行完整演示（Linux 下为真实沙箱；Windows/macOS 下为宿主模拟）
cargo run -p aion -- demo

# 交互式终端
cargo run -p aion -- repl

# 执行一次 Agent 任务
cargo run -p aion -- run --agent assistant --kind chat --input "介绍一下 AION"
cargo run -p aion -- run --agent coder --kind run --input "echo hello"

# 列出运行时加载的系统服务
cargo run -p aion -- services
```

可选配置文件 `aion.json`（放在工作目录，不存在则使用默认值）：

```json
{
  "log.level": "info",
  "storage.root": "var/storage",
  "model.default_backend": "echo"
}
```

## LLM 供应商管理（cc-switch 风格）

Web UI 的 ⚙️ Settings → LLM 面板可以维护多组供应商配置（命名档位），一键启用切换，
配置持久化在工作目录的 `aion.providers.json`（含 API Key，已在 `.gitignore` 中排除，
**请勿提交到仓库**）。

支持两种协议：

| 协议 | 请求端点 | 示例 |
|------|----------|------|
| `openai` | `POST {base_url}/chat/completions` | DeepSeek / Qwen / OpenAI / 小米 MiMo |
| `anthropic` | `POST {base_url}/v1/messages` | 智谱 GLM（`https://open.bigmodel.cn/api/anthropic`）/ Anthropic |

`anthropic` 协议的 `base_url` 不需要带 `/v1` 后缀（自动补全）；思考模型返回的
`thinking` 块会被自动跳过，仅取 `text` 块。切换/新增供应商既可在 UI 操作，也可调用 API：

```bash
# 新增并启用
curl -X POST http://127.0.0.1:18080/api/llm/providers -H 'Content-Type: application/json' -d '{
  "name": "智谱 GLM", "protocol": "anthropic",
  "base_url": "https://open.bigmodel.cn/api/anthropic",
  "api_key": "<YOUR_KEY>", "model": "glm-5.3-flash"
}'

# 列表 / 启用 / 测试 / 删除
curl http://127.0.0.1:18080/api/llm/providers
curl -X POST http://127.0.0.1:18080/api/llm/providers/<id>/activate
curl -X POST http://127.0.0.1:18080/api/llm/test -H 'Content-Type: application/json' -d '{"id":"<id>"}'
curl -X DELETE http://127.0.0.1:18080/api/llm/providers/<id>
```

服务重启时会自动恢复上一次启用的供应商档位。

## 工程结构

```
crates/
├── cordis/          # AION 核心层 — Cordis-RS Runtime
│   ├── context      # 上下文管理（一切 API 的入口）
│   ├── service      # 服务注册与发现（provide / require / 依赖注入）
│   ├── event        # 事件总线（on / once / emit + 监视通道）
│   ├── plugin       # 插件系统（独立子作用域加载）
│   ├── fiber        # Fiber 并发模型（随作用域取消）
│   ├── effect       # Effect 副作用清理（销毁时逆序执行）
│   ├── scope        # 作用域隔离（级联销毁）
│   ├── logger       # 日志系统（分级 + 环形缓冲）
│   ├── config       # 配置系统（点路径 / 深合并）
│   ├── loader       # 加载器 / 分组（App 构建器）
│   └── lifecycle    # 生命周期状态机
├── aion-adapter/    # AION 系统适配层 — Linux Adapter
│   ├── process      # clone / execve / wait（pre_exec 应用沙箱）
│   ├── fs           # open / read / write / mount
│   ├── net          # socket / connect / bind
│   ├── cgroup       # cgroup v2 管理（memory / cpu / pids）
│   ├── namespace    # unshare / setns（mnt pid net ipc uts）
│   ├── seccomp      # seccomp-BPF 白名单过滤
│   ├── device       # 设备权限 / 访问（/dev 枚举）
│   └── capability   # 权限 / Capability（PR_CAPBSET_DROP）
├── aion-services/   # AION 服务层 — System Services（基于 Cordis-RS Service）
│   ├── process      # 进程管理        ├── sandbox   # 沙箱 & 隔离
│   ├── fs           # 文件系统        ├── storage   # 存储管理（配额）
│   ├── network      # 网络管理        ├── device    # 设备管理
│   ├── terminal     # 终端 / IO       └── model     # 模型服务（LLM）
│   └── security     # 权限 & Capability 检查模型
└── aion/            # 应用层 — Agents / Apps & CLI
    ├── agents/      # Coder / Research / Assistant / Browser / Custom
    ├── demo.rs      # 调用流程示例
    ├── repl.rs      # 交互式终端
    └── boot.rs      # 运行时启动器
```

## 架构原则

| 原则 | 说明 | 代码落点 |
|------|------|----------|
| 安全隔离 | 多级隔离 Agent → Context → Service → Kernel | `cordis::scope`、`aion-adapter::namespace`、`SandboxProfile` |
| 最小权限 | 通过 Capability 控制资源访问 | `aion-services::security`、`aion-adapter::capability` |
| 可扩展性 | 插件化架构，动态加载 / 卸载 | `cordis::plugin`、`cordis::loader` |
| 高性能 | Fiber 并发模型，异步非阻塞 | `cordis::fiber`（tokio 任务托管） |
| 可观测性 | 日志、事件、指标统一管理 | `cordis::logger`、`cordis::event`（监视通道）、cgroup stats |

## 平台支持

| 能力 | Linux (root) | Linux (非 root) | Windows / macOS |
|------|:-----:|:-----:|:---------------:|
| 编译 & 测试（CI） | ✅ | ✅ | ✅ |
| 进程 / 文件 / 网络 / 终端 | ✅ | ✅ | ✅ |
| namespace 隔离 | ✅ | ➖ 自动跳过 | ❌（内存模拟或返回 Unsupported） |
| seccomp / capability 收缩 | ✅ | ➖ 自动跳过 | ❌ |
| cgroup v2 资源限制 | ✅ | 自动降级模拟（`EmulatedCgroupAdapter`） | ❌（内存模拟） |
| 进程沙箱强制执行 | ✅ `sandboxed=true` | ⚠️ 仅 `no_new_privs` | ❌（`sandboxed=false`） |

> 任何环境都不会因沙箱能力不足而运行失败。Linux 非 root、容器或无 cgroup v2 写权限
> （cgroup v1）时：cgroup 自动降级为 `EmulatedCgroupAdapter`（资源限制尽力而为，启动时
> 一次性探测 `/sys/fs/cgroup` 是否可写，不再每次 spawn 失败告警）；namespace / seccomp /
> capability 在无特权时自动跳过。`SandboxSupport`（`aion demo` 与启动日志）会如实标
> `namespace ✗ · cgroup ✗` 而非虚报。

## 已在真内核验证

完整沙箱路径（含 `unshare(2)` / `prctl(PR_SET_SECCOMP)` / 真 cgroupfs 读写 / `PR_CAPBSET_DROP`）已在 **Ubuntu 24 内核 6.8 真机上以 root 跑通**：

- `cargo test --workspace -- --test-threads=1` → 13 个测试套件（含 13 项并发压力测试）全部通过；
- `cargo run -p aion -- demo` 阶段 [4/6] 输出：
  ```
  平台沙箱能力: namespace ✓ · cgroup ✓ · seccomp ✓ · capability ✓
  spawn 完成: ... sandboxed=true
  exit code = 0（1ms）stdout: hello AION
  ```
- `DeviceService` 列出 198 个真实 `/dev` 节点。

复现方法：任意 root Linux 拉取仓库后 `cargo run -p aion -- demo` 即可看到能力报告全部 ✓。

## 开发

```bash
cargo test --workspace     # 全部测试
cargo build --workspace    # 构建
```

提交 PR 前建议本地跑通 CI 同款命令（见 `.github/workflows/ci.yml`，ubuntu + windows 双平台构建与测试）。

## License

[MIT](LICENSE) © JayCRL
