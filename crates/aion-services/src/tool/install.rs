//! `system.install` —— 按能力清单补装缺失的外部软件依赖（「能力广场」的安装原语）。
//!
//! **窄口安全**：不接受任意包名 / URL / shell——只接受一个 `capability` 名
//! （`web.view` / `file.view` / `media.view`…），装什么由 `builtin_capability_deps()`
//! 这份**编译期清单**决定，与 `app.open` 的「白名单查看器」同一思路。
//!
//! 两种安装方式：
//! - `Download` → 用户级独立二进制/归档，落到 `~/.local/bin/<to>`（零 root，moli/yt-dlp 先例）；
//! - `Apt` → 需要 root：先试 `sudo -n apt-get`（要求已配 NOPASSWD）；探测不到就给
//!   「请手动执行」提示——**本进程不持有、不接收 sudo 密码**。
//!
//! 已满足（`binaries` 任一在 PATH / `~/.local/bin` 命中）的依赖自动跳过。返回逐依赖
//! 的 `already / installed / needs_sudo / failed` 状态，供广场渲染。
//!
//! `risk: High` → 天然走现有 `run_consented` 确认门，模型主动装时需用户点同意。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use cordis::Context;
use serde_json::{json, Value};

use aion_protocol::capability::{CapabilityDep, InstallMethod};
use aion_protocol::prelude::*;
use aion_protocol::schema::{JsonSchema, JsonSchemaDocument};

use crate::capability::{builtin_capability_deps, dep_satisfied};
use crate::tool::{Tool, ToolCallScope};

/// 单个依赖安装的最终状态。
enum InstallOutcome {
    /// 探测已满足，未做任何事。
    Already,
    /// 本次装成功（下载或 apt 返回 0）。
    Installed,
    /// apt 需要 root 但无 NOPASSWD，需人手动执行。
    NeedsSudo,
    /// 失败（含 hint 尾部输出）。
    Failed(String),
}

pub struct SystemInstallTool {
    def: ToolDefinition,
}

impl SystemInstallTool {
    pub fn new() -> Self {
        Self {
            def: ToolDefinition {
                name: "system.install".into(),
                description: concat!(
                    "给一个 Capability 补装缺失的外部软件依赖（媒体播放器、看图/文档阅读器、",
                    "无头网页引擎、视频站解析器等）。只接受 capability 名（web.view / file.view / ",
                    "media.view …），AION 按内置清单决定装什么：用户级独立二进制下载到 ~/.local/bin",
                    "（免 root），系统包尝试 sudo -n apt-get（需已配 NOPASSWD，否则返回需手动执行的",
                    "命令）。已满足的依赖自动跳过。给 capability 即可，例：capability=media.view。"
                )
                .into(),
                input: JsonSchemaDocument::new(JsonSchema::Object {
                    properties: BTreeMap::from([(
                        "capability".into(),
                        Box::new(JsonSchema::String {
                            min_length: Some(1),
                            max_length: Some(64),
                            pattern: None,
                        }),
                    )]),
                    required: vec!["capability".into()],
                    additional: Box::new(JsonSchema::Any),
                }),
                output: None,
                required_caps: vec!["process:spawn".into()],
                risk: Risk::High,
            },
        }
    }
}

#[async_trait]
impl Tool for SystemInstallTool {
    fn definition(&self) -> &ToolDefinition {
        &self.def
    }

    async fn call(&self, _ctx: &Context, _scope: &ToolCallScope, args: Value) -> ToolResult {
        let cap = args
            .get("capability")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or("")
            .to_string();
        if cap.is_empty() {
            return ToolResult::error(
                aion_protocol::result::ErrorKind::InvalidInput,
                "`capability` 不能为空",
            );
        }
        // 窄口：只认内置清单里的能力名。
        let deps = match builtin_capability_deps().remove(&cap) {
            Some(d) => d,
            None => {
                return ToolResult::error(
                    aion_protocol::result::ErrorKind::NotFound,
                    format!("未知能力 `{cap}`（可安装依赖的只有内置能力）"),
                )
            }
        };

        let mut items: Vec<Value> = Vec::new();
        let mut all_ok = true;
        for dep in &deps {
            let outcome = if dep_satisfied(dep) {
                InstallOutcome::Already
            } else {
                install_dep(dep).await
            };
            // 装完复测：即便 install_dep 说成功，也以探测为准。
            let effective = match &outcome {
                InstallOutcome::Already => "already",
                InstallOutcome::Installed => {
                    if dep_satisfied(dep) {
                        "installed"
                    } else {
                        all_ok = false;
                        "failed"
                    }
                }
                InstallOutcome::NeedsSudo => {
                    all_ok = false;
                    "needs_sudo"
                }
                InstallOutcome::Failed(_) => {
                    all_ok = false;
                    "failed"
                }
            };
            let hint = match &outcome {
                InstallOutcome::Failed(h) => Some(format!("安装失败：{h}")),
                InstallOutcome::NeedsSudo => Some(apt_manual_hint(dep)),
                _ => None,
            };
            items.push(json!({
                "label": dep.label,
                "binaries": dep.binaries,
                "status": effective,
                "hint": hint,
            }));
        }

        ToolResult::success(json!({
            "capability": cap,
            "all_ok": all_ok,
            "deps": items,
        }))
    }
}

/// 装单个未满足的依赖，返回状态。
async fn install_dep(dep: &CapabilityDep) -> InstallOutcome {
    match &dep.method {
        InstallMethod::Download { url, to, extract } => {
            match download_binary(url, to, *extract).await {
                Ok(note) => {
                    let _ = note;
                    InstallOutcome::Installed
                }
                Err(e) => InstallOutcome::Failed(e),
            }
        }
        InstallMethod::Apt { packages } => apt_install(packages),
    }
}

/// apt 依赖需手动时的提示语。
fn apt_manual_hint(dep: &CapabilityDep) -> String {
    let pkgs = match &dep.method {
        InstallMethod::Apt { packages } => packages.join(" "),
        _ => String::new(),
    };
    if pkgs.is_empty() {
        "缺少系统包，请手动安装对应软件".to_string()
    } else {
        format!("需要 root：请手动执行 `sudo apt-get install -y {pkgs}`（或给 AION 配 NOPASSWD 后重试）")
    }
}

/// 用户级下载安装：落到 `~/.local/bin/<to>`，零 root。
///
/// `extract=true`：下载体是 tar 归档（moli 等），解包后按文件名 `<to>` 递归找可执行
/// 文件放到位（归档内可能带版本号目录，不能假定固定路径）。`extract=false`：下载体
/// 本身就是要装的独立二进制（yt-dlp 等），直接写盘 + chmod 755。
async fn download_binary(url: &str, to: &str, extract: bool) -> Result<String, String> {
    let home = std::env::var("HOME").map_err(|_| "无 $HOME，无法定位 ~/.local/bin".to_string())?;
    let dir = PathBuf::from(format!("{home}/.local/bin"));
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建 {dir:?} 失败：{e}"))?;
    let target = dir.join(to);

    // 下载（github release latest 会 302 到真实资产，跟随重定向）。
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(240))
        .redirect(reqwest::redirect::Policy::limited(8))
        .build()
        .map_err(|e| format!("HTTP 客户端初始化失败：{e}"))?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("下载 {url} 失败：{e}"))?;
    let status = resp.status().as_u16();
    let bytes = resp.bytes().await.map_err(|e| format!("读取响应体失败：{e}"))?;
    if status >= 400 {
        return Err(format!("HTTP {status} @ {url}"));
    }
    if bytes.is_empty() {
        return Err(format!("下载为空（{url}）"));
    }
    if bytes.len() < 1024 && !extract {
        // 独立二进制小于 1KB 基本是 404/HTML 页而非真二进制
        let head = String::from_utf8_lossy(&bytes[..bytes.len().min(200)]).into_owned();
        return Err(format!("下载体异常（仅 {} 字节）：{head}", bytes.len()));
    }

    if extract {
        extract_tar_to(&bytes, &target, to)?;
    } else {
        write_exec(&target, &bytes)?;
    }
    Ok(format!("{to} → {}", target.display()))
}

/// 把 tar 字节解包，按文件名找可执行文件放到 target。
fn extract_tar_to(bytes: &[u8], target: &Path, name: &str) -> Result<(), String> {
    let work = std::env::temp_dir().join(format!("aion-install-{}-{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| format!("创建临时目录失败：{e}"))?;
    let archive = work.join("archive.tar");
    std::fs::write(&archive, bytes).map_err(|e| format!("写临时归档失败：{e}"))?;
    let ok = run_capture("tar", &["-xf", archive.to_str().unwrap_or(""), "-C", work.to_str().unwrap_or("")], 60).0;
    let found = find_file_named(&work, name);
    let result = if !ok {
        Err("tar 解包失败".to_string())
    } else {
        match found {
            Some(src) => {
                let data = std::fs::read(&src).map_err(|e| format!("读解出的 {name} 失败：{e}"))?;
                write_exec(target, &data)?;
                Ok(())
            }
            None => Err(format!("归档里没找到名为 `{name}` 的可执行文件")),
        }
    };
    let _ = std::fs::remove_dir_all(&work);
    result
}

/// 递归找文件名为 `name` 的普通文件。
fn find_file_named(root: &Path, name: &str) -> Option<PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let rd = std::fs::read_dir(&dir).ok()?;
        for ent in rd.flatten() {
            let p = ent.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.is_file() && p.file_name().map(|f| f == name).unwrap_or(false) {
                return Some(p);
            }
        }
    }
    None
}

/// 写文件 + 可执行权限。
fn write_exec(target: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(target, bytes).map_err(|e| format!("写 {} 失败：{e}", target.display()))?;
    set_exec(target);
    Ok(())
}

#[cfg(unix)]
fn set_exec(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(m) = std::fs::metadata(p) {
        let mut perm = m.permissions();
        perm.set_mode(0o755);
        let _ = std::fs::set_permissions(p, perm);
    }
}
#[cfg(not(unix))]
fn set_exec(_p: &Path) {}

/// apt 安装：root 直接 apt-get；否则 `sudo -n`（无 NOPASSWD 会快速失败 → NeedsSudo）。
fn apt_install(packages: &[String]) -> InstallOutcome {
    if packages.is_empty() {
        return InstallOutcome::Failed("apt 依赖未声明包名".to_string());
    }
    let is_root = run_capture("id", &["-u"], 5).1.trim() == "0";
    let args_base = ["-y"];
    let full: Vec<&str> = if is_root {
        let mut a = vec!["apt-get"];
        a.push("install");
        a.extend(args_base.iter().copied());
        a.extend(packages.iter().map(|s| s.as_str()));
        a
    } else {
        let mut a = vec!["sudo", "-n", "apt-get"];
        a.push("install");
        a.extend(args_base.iter().copied());
        a.extend(packages.iter().map(|s| s.as_str()));
        a
    };
    let (prog, rest) = full.split_first().expect("args nonempty");
    let (ok, out) = run_capture(prog, rest, 300);
    if ok {
        InstallOutcome::Installed
    } else if is_root {
        InstallOutcome::Failed(tail(&out))
    } else {
        // 非 root + sudo 失败：最可能是没有 NOPASSWD（不会真去要密码）。
        InstallOutcome::NeedsSudo
    }
}

/// 同步执行外部程序，超时杀掉；返回 (exit 是否 0, stdout+stderr 合并)。
fn run_capture(program: &str, args: &[&str], secs: u64) -> (bool, String) {
    use std::io::Read;
    let child = match std::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return (false, format!("启动 {program} 失败：{e}")),
    };
    let start = Instant::now();
    let mut child = child;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(_) => break,
        }
        if start.elapsed() >= Duration::from_secs(secs) {
            let _ = child.kill();
            let _ = child.wait();
            return (false, format!("{program} 超过 {secs}s 未完成，已终止"));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let mut out = String::new();
    if let Some(mut o) = child.stdout.take() {
        let _ = o.read_to_string(&mut out);
    }
    if let Some(mut e) = child.stderr.take() {
        let mut es = String::new();
        if e.read_to_string(&mut es).is_ok() && !es.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&es);
        }
    }
    let code = child
        .try_wait()
        .ok()
        .flatten()
        .and_then(|s| s.code())
        .unwrap_or(-1);
    (code == 0, out)
}

fn tail(s: &str) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(6);
    lines[start..].join("\n")
}
