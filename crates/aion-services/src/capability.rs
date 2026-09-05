//! Capability 服务层：注册表 + Provider 解析。
//!
//! **Capability 是给 Agent 看的目标导向接口**（`web.view`），**Tool 是其下的
//! 叶子执行原语**。本模块做两件事：
//! 1. 持有每个 Capability 的 [`CapabilityDefinition`]（纯数据，协议层）；
//! 2. 持有同名 resolver：把一次能力调用（能力名 + Agent 输入）**解析**成
//!    具体叶子工具调用 [`ResolvedProvider`]（tool 名 + 重写的 arguments）。
//!
//! 现在 resolver 是 Rust 闭包（够用且诚实）；将来换全数据驱动 provider 表时，
//! 只需替换 resolver 的实现，`web.rs` 的消费点不变。

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use aion_protocol::capability::{CapabilityDefinition, CapabilityDep, InstallMethod};
use aion_protocol::schema::{JsonSchema, JsonSchemaDocument};
use aion_protocol::tool::Risk;
use serde_json::{json, Value};

/// resolver 的一次产物：落到的叶子工具 + 真正执行用的参数。
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedProvider {
    pub tool: String,
    pub arguments: Value,
}

/// 解析函数：输入 = Agent 对该能力给的参数，输出 = 落到的叶子工具调用。
pub type Resolver = dyn Fn(&Value) -> ResolvedProvider + Send + Sync;

/// Capability 注册表：定义 + 解析器成对。
#[derive(Default)]
pub struct CapabilityRegistry {
    defs: BTreeMap<String, Arc<CapabilityDefinition>>,
    resolvers: BTreeMap<String, Box<Resolver>>,
}

impl CapabilityRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一个能力（定义 + 解析器）。重名返回 Err。
    pub fn register(
        &mut self,
        def: CapabilityDefinition,
        resolver: Box<Resolver>,
    ) -> Result<(), String> {
        let name = def.name.clone();
        if self.defs.contains_key(&name) {
            return Err(format!("duplicate capability `{name}`"));
        }
        self.defs.insert(name.clone(), Arc::new(def));
        self.resolvers.insert(name, resolver);
        Ok(())
    }

    pub fn has(&self, name: &str) -> bool {
        self.defs.contains_key(name)
    }

    pub fn list(&self) -> Vec<CapabilityDefinition> {
        self.defs
            .values()
            .map(|d| (**d).clone())
            .collect()
    }

    pub fn get(&self, name: &str) -> Option<&CapabilityDefinition> {
        self.defs.get(name).map(|d| d.as_ref())
    }

    /// 把一次能力调用解析成叶子工具调用。未知能力 / 无 resolver → None。
    pub fn resolve(&self, name: &str, input: &Value) -> Option<ResolvedProvider> {
        self.resolvers.get(name).map(|r| r(input))
    }
}

/// 注册内置 Capability。当前第一个（也是试点）：**`web.view`**。
///
/// `web.view` 对应「看一个网页」这一目标。它有两个叶子 Provider：
/// - `web.fetch` —— 普通站原生重排（站点皮肤优先，快）；
/// - `web.read`  —— Moli 无头引擎，跑 JS 后取结构 / markdown（SPA / 动态站）。
/// resolver 按 `mode` 显式覆盖，或按 URL 是否命中已知站点皮肤自动选。
pub fn register_builtin_capabilities() -> CapabilityRegistry {
    let mut reg = CapabilityRegistry::new();

    let def = CapabilityDefinition {
        name: "web.view".into(),
        summary: "浏览 / 查看一个网页的内容".into(),
        description: concat!(
            "查看 http(s):// URL 对应的网页内容并展示给用户。这是浏览网页的统一入口：",
            "无需关心底层用哪个抓取引擎——AION 会按站点类型自动选最合适的实现",
            "（普通站原生重排；JS 渲染的 SPA/动态站走无头引擎取真实正文）。",
            "给 url。可选 mode=fetch（强制原生抓取重排）| structure（无头引擎取结构化正文）| ",
            "markdown（无头引擎取整页 markdown）。例：用户说“打开 react.dev 看看”→ url=https://react.dev"
        )
        .into(),
        input: JsonSchemaDocument::new(JsonSchema::Object {
            properties: BTreeMap::from([
                (
                    "url".into(),
                    Box::new(JsonSchema::String {
                        min_length: Some(8),
                        max_length: Some(2048),
                        pattern: None,
                    }),
                ),
                (
                    "mode".into(),
                    Box::new(JsonSchema::String {
                        min_length: Some(2),
                        max_length: Some(16),
                        pattern: None,
                    }),
                ),
            ]),
            required: vec!["url".into()],
            additional: Box::new(JsonSchema::Any),
        }),
        required_caps: vec!["net:fetch".into()],
        risk: Risk::Low,
        providers: vec!["web.fetch".into(), "web.read".into()],
        deps: deps_web_view(),
    };

    let resolver: Box<Resolver> = Box::new(|input: &Value| {
        let url = input.get("url").and_then(|v| v.as_str()).unwrap_or("");
        let mode = input
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        match mode {
            "fetch" => ResolvedProvider {
                tool: "web.fetch".into(),
                arguments: json!({ "url": url }),
            },
            "structure" | "moli" => ResolvedProvider {
                tool: "web.read".into(),
                arguments: json!({ "url": url, "structure": true }),
            },
            "markdown" => ResolvedProvider {
                tool: "web.read".into(),
                arguments: json!({ "url": url }),
            },
            // 默认：已知站点皮肤 → 原生重排；否则交给 Moli 引擎取结构
            _ => {
                if crate::tool::web::is_known_skin_site(url) {
                    ResolvedProvider {
                        tool: "web.fetch".into(),
                        arguments: json!({ "url": url }),
                    }
                } else {
                    ResolvedProvider {
                        tool: "web.read".into(),
                        arguments: json!({ "url": url, "structure": true }),
                    }
                }
            }
        }
    });

    reg.register(def, resolver)
        .expect("register web.view capability");

    // ======================================================================
    // file.view —— 查看 / 打开本地文件或目录（按类型自动选阅读方式）
    // ======================================================================
    //
    // 「file.read 应该是 read 然后按类型打开阅读器」：文本/代码 AION 自己当阅读器
    // （file.read → 代码围栏），目录列条目（file.list → table），图片 / 媒体 / PDF /
    // Office 交给**已装的外部程序**（app.open → 桌面直接弹 feh / vlc / zathura /
    // libreoffice）。用户已拍板：图片跟视频一样**弹外部看图窗口**，不内嵌。
    let fdef = CapabilityDefinition {
        name: "file.view".into(),
        summary: "查看 / 打开一个本地文件或目录（按类型自动选阅读方式：文本自渲染、图片/媒体/PDF/Office 交给已装看图或阅读器自动弹出、目录列表）".into(),
        description: concat!(
            "查看 path 指向的本地文件或目录。自动按文件类型决定怎么呈现：文本/代码/日志/",
            "数据 → AION 直接渲染成正文；图片 → 直接打开本机已装的看图程序（feh/eog/",
            "display…），窗口弹到桌面；目录 → 列出条目；视频/音频/PDF/Office → 直接调用",
            "本机已安装的阅读器程序（vlc/evince/libreoffice…）打开。可选 mode 强制路径：",
            "text（当文本读）/ base64（取原始字节）/ list（当目录列）/ app（只调用外部程序）。",
            "给 path 即可。"
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
                    "mode".into(),
                    Box::new(JsonSchema::String {
                        min_length: Some(2),
                        max_length: Some(12),
                        pattern: None,
                    }),
                ),
            ]),
            required: vec!["path".into()],
            additional: Box::new(JsonSchema::Any),
        }),
        required_caps: vec!["fs:read".into()],
        risk: Risk::Low,
        providers: vec!["file.read".into(), "file.list".into(), "app.open".into()],
        deps: deps_file_view(),
    };

    let fresolver: Box<Resolver> = Box::new(|input: &Value| {
        let path = input
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let mode = input
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        let list = |p: String| ResolvedProvider {
            tool: "file.list".into(),
            arguments: json!({ "path": p }),
        };
        let read = |p: String, base64: bool| ResolvedProvider {
            tool: "file.read".into(),
            arguments: if base64 {
                json!({ "path": p, "encoding": "base64" })
            } else {
                json!({ "path": p })
            },
        };
        let open = |p: String| ResolvedProvider {
            tool: "app.open".into(),
            arguments: json!({ "path": p }),
        };
        match mode {
            "list" => return list(path),
            "text" => return read(path, false),
            "base64" => return read(path, true),
            "app" => return open(path),
            _ => {}
        }
        // 默认：目录（拖尾 / 或实为目录）→ 列表
        if path.ends_with('/') || Path::new(&path).is_dir() {
            return list(path);
        }
        let ext = lower_ext(&path);
        if image_mime(&ext).is_some() {
            // 图片 → 弹已装看图程序（feh/eog/display），与视频/PDF 同一机制。想取原始
            // 字节内嵌可显式 mode=base64。
            return open(path);
        }
        if is_text_ext(&ext) {
            return read(path, false); // 文本 → 自渲染
        }
        // 媒体 / PDF / Office → 交给已装外部阅读器
        open(path)
    });

    reg.register(fdef, fresolver)
        .expect("register file.view capability");

    // ======================================================================
    // media.view —— 播放一段在线视频 / 音频（URL → 本机播放器弹窗）
    // ======================================================================
    //
    // 「放这个视频 <B站链接>」这类目标不该逼 Agent 去拼 terminal/process 命令（那会
    // 撞 High risk 确认门、命令还容易写空）。media.view 把 http(s) URL 交给 app.open
    // 叶子 → mpv/vlc 弹窗播放；站点视频由 yt-dlp 解析出真实流。Low risk，不设确认门。
    let mdef = CapabilityDefinition {
        name: "media.view".into(),
        summary: "播放一段在线视频 / 音频（给 http(s) URL，用本机播放器弹窗放，支持 B 站等视频站）".into(),
        description: concat!(
            "播放 url 指向的在线视频或音频，播放器窗口直接弹到桌面。这是看在线媒体内容的统一入口：",
            "无需自己拼命令行、无需抓流——AION 用本机已装的播放器（mpv/vlc）打开，视频站页面由 ",
            "yt-dlp 自动解析出真实媒体流。给 url 即可。例：用户说“放这个视频 ",
            "https://www.bilibili.com/video/BVxxxx” → url 填这个链接。"
        )
        .into(),
        input: JsonSchemaDocument::new(JsonSchema::Object {
            properties: BTreeMap::from([(
                "url".into(),
                Box::new(JsonSchema::String {
                    min_length: Some(8),
                    max_length: Some(4096),
                    pattern: None,
                }),
            )]),
            required: vec!["url".into()],
            additional: Box::new(JsonSchema::Any),
        }),
        required_caps: vec!["process:spawn".into()],
        risk: Risk::Low,
        providers: vec!["app.open".into()],
        deps: deps_media_view(),
    };

    let mresolver: Box<Resolver> = Box::new(|input: &Value| {
        let url = input
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        ResolvedProvider {
            tool: "app.open".into(),
            arguments: json!({ "path": url }),
        }
    });

    reg.register(mdef, mresolver)
        .expect("register media.view capability");
    reg
}

/// 文本 / 代码类扩展名（AION 自己当阅读器）。
pub fn is_text_ext(ext: &str) -> bool {
    matches!(
        ext,
        "txt" | "md" | "markdown" | "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "json"
            | "toml" | "yaml" | "yml" | "sh" | "bash" | "zsh" | "c" | "h" | "cpp" | "hpp"
            | "cc" | "java" | "go" | "rb" | "php" | "css" | "html" | "htm" | "xml" | "ini"
            | "conf" | "log" | "csv" | "sql" | "env" | "properties" | "lock" | "gitignore"
    )
}

/// 图片扩展名 → MIME（原生 image 块用，data URL 前缀）。
pub fn image_mime(ext: &str) -> Option<&'static str> {
    match ext {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "svg" => Some("image/svg+xml"),
        "bmp" => Some("image/bmp"),
        "ico" => Some("image/x-icon"),
        "avif" => Some("image/avif"),
        _ => None,
    }
}

/// 媒体 / 文档 / 办公扩展名（交给 app.open 外部阅读器的类型）。
pub fn is_external_ext(ext: &str) -> bool {
    matches!(
        ext,
        "mp4" | "mkv" | "avi" | "mov" | "webm" | "m4v" | "mp3" | "flac" | "wav" | "m4a"
            | "ogg" | "opus" | "aac" | "pdf" | "ps" | "djvu" | "odt" | "doc" | "docx"
            | "xls" | "xlsx" | "ppt" | "pptx" | "odp" | "epub"
    )
}

/// launch 横幅用的图标：图片 → 🖼️，媒体 → 🎬，办公/电子书 → 📊，其余文档 → 📄。
pub fn viewer_icon(ext: &str) -> &'static str {
    if matches!(
        ext,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "ico" | "avif" | "tif"
            | "tiff" | "heic"
    ) {
        "🖼️"
    } else if matches!(
        ext,
        "mp4" | "mkv" | "avi" | "mov" | "webm" | "m4v" | "mp3" | "flac" | "wav" | "m4a"
            | "ogg" | "opus" | "aac"
    ) {
        "🎬"
    } else if matches!(
        ext,
        "odt" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "odp" | "epub"
    ) {
        "📊"
    } else {
        "📄"
    }
}

/// 取小写扩展名（不含点）：`/a/b.X` → `"x"`，无扩展名 → `""`。
pub fn lower_ext(path: &str) -> String {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

// ======================================================================
// 能力依赖清单（广场展示 + system.install 安装器共用，single source of truth）
// ======================================================================
//
// 每项 CapabilityDep：`binaries` 任一在 PATH / ~/.local/bin 命中可执行即视为已满足；
// 未满足时按 `method` 补装。下载类走用户级 `~/.local/bin`（零 root，moli/yt-dlp 先例）；
// apt 类需 sudo——安装器只尝试 `sudo -n`（NOPASSWD），否则给手动提示，不在本进程持密码。

/// moli 无头引擎（web.read provider 的底层二进制）——GitHub release tar 归档。
const MOLI_URL: &str =
    "https://github.com/lexmount/moli/releases/latest/download/moli-x86_64-unknown-linux-gnu.tar";
/// yt-dlp 独立二进制（media.view 把 B 站等视频站页面解析成直链）。
const YTDLP_URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp";

/// 媒体播放器：media.view 弹窗播放 / file.view 打开本地视频音频。
fn dep_media_player() -> CapabilityDep {
    CapabilityDep {
        label: "媒体播放器".into(),
        binaries: vec!["mpv".into(), "ffplay".into(), "vlc".into(), "mplayer".into()],
        method: InstallMethod::Apt {
            packages: vec!["mpv".into()],
        },
    }
}

/// 视频站解析器：把 B 站等页面 URL 解成真实媒体流（独立二进制，零 root）。
fn dep_ytdlp() -> CapabilityDep {
    CapabilityDep {
        label: "视频站解析 yt-dlp".into(),
        binaries: vec!["yt-dlp".into()],
        method: InstallMethod::Download {
            url: YTDLP_URL.into(),
            to: "yt-dlp".into(),
            extract: false,
        },
    }
}

/// 看图程序（file.view 打开图片文件）。
fn dep_image_viewer() -> CapabilityDep {
    CapabilityDep {
        label: "看图程序".into(),
        binaries: vec!["feh".into(), "eog".into(), "display".into(), "qimgv".into()],
        method: InstallMethod::Apt {
            packages: vec!["feh".into()],
        },
    }
}

/// 文档阅读器（file.view 打开 PDF/DJVU）。
fn dep_doc_viewer() -> CapabilityDep {
    CapabilityDep {
        label: "文档阅读器".into(),
        binaries: vec!["zathura".into(), "evince".into(), "okular".into(), "mupdf".into()],
        method: InstallMethod::Apt {
            packages: vec!["zathura".into()],
        },
    }
}

/// 无头网页引擎（web.view → web.read provider 的底层二进制）。
fn dep_moli() -> CapabilityDep {
    CapabilityDep {
        label: "无头网页引擎 moli".into(),
        binaries: vec!["moli".into()],
        method: InstallMethod::Download {
            url: MOLI_URL.into(),
            to: "moli".into(),
            extract: true,
        },
    }
}

/// 办公套件查看器（file.view 打开 docx/xlsx/pptx…）。此前只出现在 app.open 的
/// viewer_candidates 白名单里，缺 `CapabilityDep` → 广场扫不到、system.install 装不了。
/// binaries 与白名单同源（libreoffice/soffice/wps/onlyoffice），apt 优先 libreoffice。
fn dep_office_viewer() -> CapabilityDep {
    CapabilityDep {
        label: "办公套件查看器".into(),
        binaries: vec![
            "libreoffice".into(),
            "soffice".into(),
            "wps".into(),
            "onlyoffice-desktopeditors".into(),
        ],
        method: InstallMethod::Apt {
            packages: vec!["libreoffice".into()],
        },
    }
}

fn deps_web_view() -> Vec<CapabilityDep> {
    vec![dep_moli()]
}
fn deps_file_view() -> Vec<CapabilityDep> {
    vec![
        dep_image_viewer(),
        dep_doc_viewer(),
        dep_media_player(),
        dep_office_viewer(),
    ]
}
fn deps_media_view() -> Vec<CapabilityDep> {
    vec![dep_media_player(), dep_ytdlp()]
}

/// 内置能力的依赖清单（能力名 → 依赖表）。
///
/// `system.install` 与 `/api/capabilities/:name/install` 只允许**按这份编译期清单**装依赖，
/// 不接受任意包名/URL——窄口安全原语（与 app.open 同一思路）。
pub fn builtin_capability_deps() -> BTreeMap<String, Vec<CapabilityDep>> {
    let mut m = BTreeMap::new();
    m.insert("web.view".into(), deps_web_view());
    m.insert("file.view".into(), deps_file_view());
    m.insert("media.view".into(), deps_media_view());
    m
}

/// 判断某能力是否已满足全部运行依赖（没列出的能力视为满足）。
pub fn capability_deps_satisfied(cap: &str) -> bool {
    builtin_capability_deps()
        .get(cap)
        .map(|deps| deps.iter().all(dep_satisfied))
        .unwrap_or(true)
}

/// 判断单个依赖是否已满足：`binaries` 任一命中 PATH / `~/.local/bin`。
pub fn dep_satisfied(dep: &CapabilityDep) -> bool {
    let names: Vec<&str> = dep.binaries.iter().map(|s| s.as_str()).collect();
    crate::tool::app::which_bin(&names).is_some()
}

/// 单个依赖命中的**完整路径**（`binaries` 里第一个在 PATH / `~/.local/bin` 探到的）；
/// 未满足 → None。web `/api/capabilities` 给广场展示「装在哪」用。判定与
/// [`dep_satisfied`] 同一套（`which_bin_path`）。
pub fn dep_path(dep: &CapabilityDep) -> Option<String> {
    let names: Vec<&str> = dep.binaries.iter().map(|s| s.as_str()).collect();
    crate::tool::app::which_bin_path(&names).map(|p| p.display().to_string())
}
