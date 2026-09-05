//! `app.open` —— 用机器上**已装的外部阅读器程序**打开本地文件。
//!
//! 这是 Capability `file.view` 底下「交给外部程序」那一路的 Provider：AION 按文件
//! 扩展名从白名单里挑候选查看器（vlc / evince / libreoffice …），探到第一个可执行的
//! 就 `spawn` 它、把文件路径当唯一参数递过去。子进程**继承本服务环境**（AION 跑在
//! lightdm 图形会话里，故 `DISPLAY` 正确），窗口直接弹到桌面；本工具不等它、不管它
//! 生命周期（与 `run_moli` 同一先例）。
//!
//! 刻意是**窄口**：不接受任意 argv，无 shell、无管道、无 flag 透传——只放行白名单
//! 查看器 + 单个文件路径。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use aion_protocol::prelude::*;
use aion_protocol::schema::{JsonSchema, JsonSchemaDocument};

use crate::tool::{Tool, ToolCallScope};

use async_trait::async_trait;
use cordis::Context;
use serde_json::{json, Value};

/// 扩展名 → 候选查看器（白名单；顺序即优先级）。空 = 无匹配，走 `xdg-open` 兜底。
fn viewer_candidates(ext: &str) -> Vec<&'static str> {
    match ext {
        // 媒体：能放视频 / 音频的播放器
        "mp4" | "mkv" | "avi" | "mov" | "webm" | "m4v" | "mp3" | "flac" | "wav" | "m4a"
        | "ogg" | "opus" | "aac" => vec!["vlc", "mpv", "ffplay", "mplayer"],
        // 文档：PDF / PostScript / DjVu
        "pdf" | "ps" | "djvu" => vec!["evince", "zathura", "okular", "mupdf", "qpdfview"],
        // 办公 / 电子书
        "odt" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "odp" | "epub" => {
            vec!["libreoffice", "soffice", "wps", "onlyoffice-desktopeditors"]
        }
        _ => Vec::new(),
    }
}

/// 通用 `which`：$PATH 各目录里探第一个可执行；绝对路径直接查。全无 → None。
fn which_bin(names: &[&str]) -> Option<String> {
    let path_env = std::env::var("PATH").unwrap_or_default();
    let dirs: Vec<PathBuf> = path_env
        .split(':')
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .collect();
    for name in names {
        let p = Path::new(name);
        if p.is_absolute() {
            if is_exec(p) {
                return Some(name.to_string());
            }
            continue;
        }
        for d in &dirs {
            let cand = d.join(name);
            if is_exec(&cand) {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// 确保子进程能连到桌面 X server。
///
/// AION web 常以 user service 启动（systemd session scope，parent pid=1），env 里**没有
/// DISPLAY**——裸 `spawn` 一个 GUI 程序会 headless 跑、窗口不出现。kiosk 的 X server 在
/// :0（lightdm 自动登录 wust_1）。若继承的 env 已有 DISPLAY（后端直接在 X 会话里跑）就
/// 原样用；否则补 `DISPLAY=:0`（可用 `AION_DISPLAY` 覆盖）+ `~/.Xauthority`。
fn ensure_display_env(cmd: &mut std::process::Command) {
    let has_display = std::env::var("DISPLAY")
        .map(|d| !d.trim().is_empty())
        .unwrap_or(false);
    if has_display {
        return;
    }
    let disp = std::env::var("AION_DISPLAY").unwrap_or_else(|_| ":0".into());
    cmd.env("DISPLAY", disp);
    if std::env::var("XAUTHORITY").is_err() {
        if let Ok(home) = std::env::var("HOME") {
            let xa = format!("{home}/.Xauthority");
            if Path::new(&xa).exists() {
                cmd.env("XAUTHORITY", &xa);
            }
        }
    }
}

fn is_exec(p: &Path) -> bool {
    match std::fs::metadata(p) {
        Ok(m) if m.is_file() => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                m.permissions().mode() & 0o111 != 0
            }
            #[cfg(not(unix))]
            {
                true
            }
        }
        _ => false,
    }
}

pub struct AppOpenTool {
    def: ToolDefinition,
}

impl AppOpenTool {
    pub fn new() -> Self {
        Self {
            def: ToolDefinition {
                name: "app.open".into(),
                description: concat!(
                    "用本机已安装的阅读器程序打开一个本地文件：按扩展名自动选播放器/阅读器",
                    "（媒体→vlc/mpv，PDF→evince/zathura，Office→libreoffice），找到就启动并把",
                    "窗口弹到桌面。可选 app= 显式指定白名单内程序。这是 file.view 内部的打开原语。"
                )
                .into(),
                input: JsonSchemaDocument::new(JsonSchema::Object {
                    properties: BTreeMap::from([
                        (
                            "path".into(),
                            Box::new(JsonSchema::String {
                                min_length: Some(1),
                                max_length: Some(4096),
                                pattern: None,
                            }),
                        ),
                        (
                            "app".into(),
                            Box::new(JsonSchema::String {
                                min_length: Some(1),
                                max_length: Some(64),
                                pattern: None,
                            }),
                        ),
                    ]),
                    required: vec!["path".into()],
                    additional: Box::new(JsonSchema::Any),
                }),
                output: None,
                required_caps: vec!["process:spawn".into()],
                risk: Risk::Low,
            },
        }
    }
}

#[async_trait]
impl Tool for AppOpenTool {
    fn definition(&self) -> &ToolDefinition {
        &self.def
    }

    async fn call(&self, _ctx: &Context, _scope: &ToolCallScope, args: Value) -> ToolResult {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or("")
            .to_string();
        if path.is_empty() {
            return ToolResult::error(
                aion_protocol::result::ErrorKind::InvalidInput,
                "`path` 不能为空",
            );
        }
        let ext = Path::new(&path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();

        // 候选顺序：app= 显式（须在该扩展名的白名单内）> 扩展名映射 > xdg-open 兜底。
        let mut names: Vec<&str> = Vec::new();
        if let Some(app) = args
            .get("app")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let allowed = viewer_candidates(&ext);
            if allowed.contains(&app) {
                names.push(app);
            } else {
                return ToolResult::error(
                    aion_protocol::result::ErrorKind::InvalidInput,
                    format!("`app`={app} 不在白名单；.{ext} 的可用候选为 {allowed:?}"),
                );
            }
        } else {
            names = viewer_candidates(&ext);
        }
        let fallback = "xdg-open";
        if names.is_empty() {
            names.push(fallback);
        }

        let bin = which_bin(&names).or_else(|| which_bin(&[fallback]));
        let Some(bin) = bin else {
            return ToolResult::error(
                aion_protocol::result::ErrorKind::NotFound,
                format!("未找到能打开 .{ext} 文件的程序（试过 {names:?}）；请安装 vlc/evince/libreoffice"),
            );
        };

        let mut cmd = std::process::Command::new(&bin);
        cmd.arg(&path);
        ensure_display_env(&mut cmd);
        match cmd.spawn() {
            Ok(_) => ToolResult::success(json!({
                "path": path,
                "app": bin,
                "ext": ext,
                "opened": true,
            })),
            Err(e) => ToolResult::error(
                aion_protocol::result::ErrorKind::Internal,
                format!("启动阅读器 `{bin}` 失败：{e}"),
            ),
        }
    }
}
