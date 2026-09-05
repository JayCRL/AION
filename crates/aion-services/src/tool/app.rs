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

/// 媒体播放器白名单：也用于 http(s) 在线媒体 URL（站点视频交给 yt-dlp 解析）。
const MEDIA_PLAYERS: &[&str] = &["vlc", "mpv", "ffplay", "mplayer"];

/// 冒充浏览器的 UA——B 站等 CDN 边缘防盗链要求「Referer + 浏览器 UA」双全，缺一 403
/// （实测: no-header/referer-only/UA-only 全 403，referer+UA → 200）。
const BROWSER_UA: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36";

/// 扩展名 → 候选查看器（白名单；顺序即优先级）。空 = 无匹配，走 `xdg-open` 兜底。
fn viewer_candidates(ext: &str) -> Vec<&'static str> {
    match ext {
        // 图片：看图程序
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "ico" | "avif"
        | "tif" | "tiff" | "heic" => {
            vec!["feh", "display", "eog", "ristretto", "gpicview", "qimgv", "viewnior"]
        }
        // 媒体：能放视频 / 音频的播放器
        "mp4" | "mkv" | "avi" | "mov" | "webm" | "m4v" | "mp3" | "flac" | "wav" | "m4a"
        | "ogg" | "opus" | "aac" => MEDIA_PLAYERS.to_vec(),
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

/// 定位 yt-dlp 二进制：env `AION_YTDLP` → `$HOME/.local/bin/yt-dlp` → PATH 上的 `yt-dlp`。
///
/// 与 `moli_bin()`（web.rs）同一先例。为什么不能裸 `Command::new("yt-dlp")`：B 站 wbi 签名
/// 频繁改版，发行版源里的老版（如 /usr/bin/yt-dlp 2022.04.08）会对 B 站**稳定 412**——解析直链
/// 每次都失败，app.open 只能回退把原始页面 URL 丢给播放器 → mpv 加载即退。用户级新版独立二进制
/// 通常装在 `~/.local/bin/yt-dlp`，须优先命中它。
fn ytdlp_bin() -> String {
    if let Ok(p) = std::env::var("AION_YTDLP") {
        if !p.trim().is_empty() {
            return p.trim().to_string();
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let cand = format!("{home}/.local/bin/yt-dlp");
        if std::path::Path::new(&cand).exists() {
            return cand;
        }
    }
    "yt-dlp".to_string()
}

/// 用 yt-dlp 把站点页面 URL 解析成可直接播放的媒体直链。
///
/// B 站等视频站的抗爬会**随机 412**：同一 URL 这次被拒、下次就成。故整段最多尝试
/// `MAX_TRIES` 次，每次起一个全新 yt-dlp 进程（= 全新 buvid cookie，等于一次新机会）。
/// yt-dlp 缺失 / 全部失败 / 超时 → None，调用方回退原 URL（纯直链媒体 mpv 仍能直接放）。
fn resolve_media_url(raw: &str) -> Option<String> {
    use std::io::Read;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    const MAX_TRIES: u32 = 4; // 412 是概率性失败，重试几次即收敛（成功那跑 ~1s）
    const PER_TRY: Duration = Duration::from_secs(10);

    for _ in 0..MAX_TRIES {
        let Ok(mut child) = std::process::Command::new(ytdlp_bin())
            // 纯视频流 bv[height<=720]：kiosk 无音频设备——带音轨的流会让 mpv 因初始化音频失败而
        // 直接中止(Errors when loading file)，纯视频流无音轨、不初始化音频，静音照放。
        .args([
                "-g",
                "--no-warnings",
                "--no-playlist",
                "--format",
                "bv[height<=720]/best",
                raw,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        else {
            return None;
        };
        let start = Instant::now();
        let mut got_url = false;
        loop {
            match child.try_wait() {
                Ok(Some(st)) if st.success() => {
                    got_url = true;
                    break;
                }
                Ok(Some(_)) => break, // 本次失败（多半 412）→ 外层重试
                Ok(None) => {}
                Err(_) => break,
            }
            if start.elapsed() >= PER_TRY {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        if !got_url {
            continue;
        }
        let mut s = String::new();
        let mut out = child.stdout.take()?;
        out.read_to_string(&mut s).ok()?;
        if let Some(url) = s.lines().map(str::trim).find(|l| {
            !l.is_empty() && (l.starts_with("http://") || l.starts_with("https://"))
        }) {
            return Some(url.to_string());
        }
    }
    None
}

/// 取一个 http(s) URL 的 origin（`scheme://host`），无 scheme / host 返回 None。
fn origin_of(raw: &str) -> Option<String> {
    let s = raw.trim();
    let scheme_end = s.find("://")?;
    let scheme = &s[..scheme_end];
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let after = &s[scheme_end + 3..];
    let host_end = after
        .find(|c| c == '/' || c == '?' || c == '#')
        .unwrap_or(after.len());
    let host = &after[..host_end];
    if host.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{host}"))
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
                    "用本机已安装的阅读器程序打开一个本地文件：按扩展名自动选播放器/看图器/阅读器",
                    "（图片→feh/eog，媒体→vlc/mpv，PDF→zathura，Office→libreoffice），找到就启动并把",
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
        // http(s) 在线媒体 URL：不按扩展名分派，直接走媒体播放器白名单，并先经 yt-dlp
        // 解出可播放直链（站点视频 B 站/优酷…）再交给播放器；解析失败则回退原 URL。
        let is_url = path.starts_with("http://") || path.starts_with("https://");
        let ext = Path::new(&path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let allowed: Vec<&str> = if is_url {
            MEDIA_PLAYERS.to_vec()
        } else {
            viewer_candidates(&ext)
        };

        // 候选顺序：app= 显式（须在对应的白名单内）> 类型映射 > xdg-open 兜底(仅本地文件)。
        let mut names: Vec<&str> = Vec::new();
        if let Some(app) = args
            .get("app")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if allowed.contains(&app) {
                names.push(app);
            } else {
                return ToolResult::error(
                    aion_protocol::result::ErrorKind::InvalidInput,
                    format!("`app`={app} 不在白名单；{allowed:?} 之外不可用（{ext}）"),
                );
            }
        } else {
            names = allowed.clone();
        }
        let fallback = "xdg-open";
        if names.is_empty() && !is_url {
            names.push(fallback);
        }
        let label = if is_url { "在线媒体 URL" } else { &ext };

        let bin = if is_url {
            which_bin(&names)
        } else {
            which_bin(&names).or_else(|| which_bin(&[fallback]))
        };
        let Some(bin) = bin else {
            return ToolResult::error(
                aion_protocol::result::ErrorKind::NotFound,
                format!("未找到能打开 {label} 的程序（试过 {names:?}）；媒体请装 vlc/mpv，文件请装 feh/zathura/libreoffice"),
            );
        };

        // URL 先经 yt-dlp 解直链（站点视频才能放）；失败退回原 URL（纯直链媒体仍可）。
        let mut play_arg = path.clone();
        if is_url {
            if let Some(direct) = resolve_media_url(&path) {
                play_arg = direct;
            }
        }
        let mut cmd = std::process::Command::new(&bin);
        cmd.arg(&play_arg);
        ensure_display_env(&mut cmd);
        // 站点直链普遍防盗链：B 站 m4s 不带 Referer 裸放会 403。mpv 用 --http-header-fields
        // 附上原页面 URL 的 origin（对不需要防盗链的纯直链媒体也无害）。只在「解析出真直链」
        // 时加——回退原页面 URL 时加了也没用（页面本身还须过反爬）。
        if is_url && play_arg != path {
            let is_mpv = Path::new(&bin)
                .file_name()
                .map(|f| f == "mpv")
                .unwrap_or(false);
            if is_mpv {
                // 禁掉 mpv 内建 ytdl_hook：直链已由 yt-dlp 解析好，直接打开即可。mpv 0.34 的
                // hook 会在 mpv 首次探测不顺时拿直链 URL 二次解析，把超长查询串(如 B 站 m4s
                // 的 buvid3/...参数)拆坏成 `'buvid3=...' is not a valid URL` → 秒退。实测
                // `--ytdl=no` 后 mpv 直连直链可完整放到底。
                cmd.arg("--ytdl=no");
                if let Some(origin) = origin_of(&path) {
                    cmd.arg(format!("--user-agent={BROWSER_UA}"));
                    cmd.arg(format!("--http-header-fields=Referer: {origin}/"));
                }
            }
        }
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
