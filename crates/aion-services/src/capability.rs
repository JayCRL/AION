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

use aion_protocol::capability::CapabilityDefinition;
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
