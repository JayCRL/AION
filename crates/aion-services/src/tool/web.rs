//! `web.fetch` —— AION 当「浏览器层」：抓网页正文 → 原生卡片数据。
//!
//! 不进浏览器、不渲染像素：AION 自己发起 HTTP(S) 请求（reqwest + rustls），
//! 把页面的 `<title>` / meta description / 正文锚点链接提取成结构化 JSON，
//! 交给前端用原生 UIBlock（markdown 卡片）展示。用户可对返回的链接 URL
//! 继续 `web.fetch`，逐层深入 = 「AION 在网页里自己导航」。
//!
//! 与 `NetworkService`（裸 TCP、仅 http://、走内核白名单）不同：这里是
//! 面向公众站点的浏览抓取，故用系统级 reqwest + rustls 直连，不经过
//! 内核网络白名单（web 会话的 `developer_sec` 已 net `*`）。
//! capability 用 `net:fetch` 单独把关「能否发起出站抓取」。

use std::collections::BTreeMap;
use std::time::Duration;

use aion_protocol::prelude::*;
use aion_protocol::schema::{JsonSchema, JsonSchemaDocument};

use crate::tool::{Tool, ToolCallScope};

use async_trait::async_trait;
use cordis::Context;
use serde_json::{json, Value};

/// 最大响应体（防抓个无限流把服务打爆）。
const MAX_BODY: usize = 4 * 1024 * 1024;
/// 一个页面最多取多少条链接。
const MAX_LINKS: usize = 24;
/// 浏览器 UA，部分站点没 UA 会拒。
const UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36 AION/0.1";

pub struct WebFetchTool {
    def: ToolDefinition,
}

impl WebFetchTool {
    pub fn new() -> Self {
        Self {
            def: ToolDefinition {
                name: "web.fetch".into(),
                description: "抓取一个网页并返回结构化内容（标题/描述/链接），供 AION 原生卡片展示。给 http(s):// URL。例：用户说“打开百度 / 弹一个百度”→ url=https://www.baidu.com".into(),
                input: JsonSchemaDocument::new(JsonSchema::Object {
                    properties: BTreeMap::from([(
                        "url".into(),
                        Box::new(JsonSchema::String {
                            min_length: Some(8),
                            max_length: Some(2048),
                            pattern: None,
                        }),
                    )]),
                    required: vec!["url".into()],
                    additional: Box::new(JsonSchema::Any),
                }),
                output: None,
                required_caps: vec!["net:fetch".into()],
                risk: Risk::Low,
            },
        }
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn definition(&self) -> &ToolDefinition {
        &self.def
    }

    async fn call(
        &self,
        _ctx: &cordis::Context,
        _scope: &ToolCallScope,
        args: Value,
    ) -> ToolResult {
        let url = args
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return ToolResult::error(
                ErrorKind::InvalidInput,
                format!("web.fetch 只支持 http(s):// URL，收到：{url}"),
            );
        }
        match fetch_page(url).await {
            Ok(page) => ToolResult::success(page),
            Err(msg) => ToolResult::error(ErrorKind::ExternalService, msg),
        }
    }
}

// ---------------------------------------------------------------------------
// `web.read` —— 真·读任意页：壳调 Moli（无头浏览器引擎，跑 JS + 原生 DOM）
// ---------------------------------------------------------------------------
//
// `web.fetch` 只做单次匿名 GET，SPA/JS 渲染站拿到的是空壳，登录墙/验证码站
// 更是拿不到内容。`web.read` 把抓取交给 Moli——一个 Rust 自研的无头浏览器
// 内核：它真实执行页面 JavaScript、维护 DOM、可按需等加载完成，再把「跑完
// JS 之后的真实内容」渲染成 Markdown（或语义树）吐回来。AION 不进浏览器、
// 不碰像素，只消费结构文本。
//
// Moli 二进制定位：env `AION_MOLI` → `$HOME/.local/bin/moli` → PATH 上的 `moli`。
pub struct WebReadTool {
    def: ToolDefinition,
}

impl WebReadTool {
    pub fn new() -> Self {
        Self {
            def: ToolDefinition {
                name: "web.read".into(),
                description: concat!(
                    "用 Moli 无头浏览器引擎读取网页（真实执行页面 JavaScript 并等其加载完），",
                    "返回渲染成 Markdown 的正文。适合 JS 渲染的 SPA/动态站、或普通抓取拿到空壳的页面。",
                    "给 http(s):// URL。可选 dump=markdown（默认）| semantic_tree_text。",
                    "例：用户说“打开这个动态页面看看内容”→ url=…"
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
                            "dump".into(),
                            Box::new(JsonSchema::String {
                                min_length: Some(3),
                                max_length: Some(32),
                                pattern: None,
                            }),
                        ),
                    ]),
                    required: vec!["url".into()],
                    additional: Box::new(JsonSchema::Any),
                }),
                output: None,
                required_caps: vec!["net:fetch".into()],
                risk: Risk::Low,
            },
        }
    }
}

#[async_trait]
impl Tool for WebReadTool {
    fn definition(&self) -> &ToolDefinition {
        &self.def
    }

    async fn call(
        &self,
        _ctx: &cordis::Context,
        _scope: &ToolCallScope,
        args: Value,
    ) -> ToolResult {
        let url = args
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return ToolResult::error(
                ErrorKind::InvalidInput,
                format!("web.read 只支持 http(s):// URL，收到：{url}"),
            );
        }
        let dump = args
            .get("dump")
            .and_then(|v| v.as_str())
            .unwrap_or("markdown")
            .trim();
        let dump = match dump {
            "semantic_tree_text" | "semantic_tree" => "semantic_tree_text",
            _ => "markdown",
        };
        match read_page_moli(url, dump) {
            Ok(page) => ToolResult::success(page),
            Err(msg) => ToolResult::error(ErrorKind::ExternalService, msg),
        }
    }
}

/// 调 `moli fetch --dump <dump> --wait-until done <url>`，把 stdout 包装成
/// 结构化 JSON（内容超长截断到 `MOLI_MAX_CHARS`）。
fn read_page_moli(url: &str, dump: &str) -> Result<Value, String> {
    const MOLI_MAX_CHARS: usize = 60_000;
    let args = [
        "fetch",
        "--dump",
        dump,
        "--wait-until",
        "done",
        "--timeout",
        "25000",
        url,
    ];
    let out = run_moli(&args, 60_000)?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).into_owned();
        let lower = err.to_ascii_lowercase();
        // 该地址返回非 HTML 原始数据（接口/下载）时 Moli 会拒做 markdown ——
        // 翻译成人话，并指向正确姿势（取数据走 web.fetch 或直调其 API，而非 web.read）。
        if lower.contains("raw download") || lower.contains("--dump json") {
            return Err(format!(
                "{url} 返回的是非 HTML 原始数据（API 接口 / 文件下载），不是可渲染网页。\
                 \n要读它的数据请改用 web.fetch，或直调该接口（带登录态）。"
            ));
        }
        let snippet: String = collapse(&err).chars().take(300).collect();
        let hint = if snippet.is_empty() {
            "无错误输出".to_string()
        } else {
            snippet
        };
        return Err(format!(
            "Moli 读取失败(exit {}): {hint}",
            out.status.code().unwrap_or(-1)
        ));
    }
    let body = String::from_utf8_lossy(&out.stdout).into_owned();
    let body = body.trim().to_string();
    let total = body.chars().count();
    let body = cap(body, MOLI_MAX_CHARS);
    let empty = body.trim().is_empty();
    Ok(json!({
        "engine": "moli",
        "url": url,
        "dump": dump,
        "content": body,
        "chars": total,
        "truncated": total > MOLI_MAX_CHARS,
        "empty": empty,
        "hint": if empty {
            "Moli 拿到空内容 —— 可能是登录墙/401/反爬，或该地址本就无正文（如纯 API 站）。"
        } else {
            ""
        },
    }))
}

/// 定位 Moli 二进制。
fn moli_bin() -> String {
    if let Ok(p) = std::env::var("AION_MOLI") {
        if !p.trim().is_empty() {
            return p.trim().to_string();
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let cand = format!("{home}/.local/bin/moli");
        if std::path::Path::new(&cand).exists() {
            return cand;
        }
    }
    "moli".to_string()
}

/// 起一个 Moli 子进程并带超时地等它结束。
///
/// 关键超时控制靠传 `--timeout` 让 Moli 自尽；这里的 `max_ms` 只是兜底看门狗，
/// 兜底触发时进程可能短暂残留，但 Moli 自带超时会先到，故实际很少走到。
fn run_moli(args: &[&str], max_ms: u64) -> Result<std::process::Output, String> {
    let bin = moli_bin();
    let mut child = std::process::Command::new(&bin)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| {
            format!(
                "启动 Moli 失败(`{bin}`): {e} —— 需先在机器上装 Moli(见 AION_MOLI env)"
            )
        })?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(Duration::from_millis(max_ms)) {
        Ok(Ok(o)) => Ok(o),
        Ok(Err(e)) => Err(format!("读取 Moli 输出失败：{e}")),
        Err(_) => Err(format!("Moli 超过 {max_ms}ms 未返回（页面可能卡死），已放弃。")),
    }
}

/// 抓取 + 提取，出错返回人类可读的 Err 串。
async fn fetch_page(url: &str) -> Result<Value, String> {
    let client = reqwest::Client::builder()
        .user_agent(UA)
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::limited(6))
        .build()
        .map_err(|e| format!("HTTP 客户端初始化失败：{e}"))?;
    let resp = client.get(url).send().await.map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let final_url = resp.url().to_string();
    let ct = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    let n = bytes.len();
    if n > MAX_BODY {
        return Err(format!("响应 {n} 字节超过上限，已中止"));
    }
    if status >= 400 {
        return Err(format!("HTTP {status} @ {final_url}"));
    }
    // 解码：目前按 UTF-8（百度等主流站点是 UTF-8）；GBK 老站会乱，属已知限制。
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let html = preprocess(&text);
    let title = cap(unescape(&strip_text(&extract_title(&html))), 200);
    let description = cap(unescape(&strip_text(&extract_meta(&html, &["description", "og:description"]))), 400);
    let links = extract_links(&html, &final_url);
    // —— 站点感知重建：导航/logo/搜索框/页脚 + 品牌皮肤 ——
    let brand = brand_for(&final_url);
    let logo = find_logo(&html, &final_url).or_else(|| {
        let og = extract_meta(&html, &["og:image"]);
        if og.is_empty() {
            None
        } else {
            resolve_abs(&og, &final_url, &origin_of(&final_url), &scheme_of(&final_url))
        }
    });
    let search = find_search(&html);
    let (nav, footer) = split_nav_footer(&links, &final_url);

    Ok(json!({
        "url": final_url,
        "status": status,
        "content_type": ct,
        "bytes": n,
        "title": title,
        "description": description,
        "links": links,
        // 结构：站点皮肤入口（渲染成原生"网页感"卡片）
        "brand_name": brand.0,
        "brand_color": brand.1,
        "logo": logo,
        "nav": nav,
        "search": search,
        "footer": footer,
    }))
}

// ---------------------------------------------------------------------------
// 站点感知：品牌皮肤 + 结构提取（"逐站加皮肤"的可插拔点）
// ---------------------------------------------------------------------------

/// 品牌皮肤表：host 片段 → (显示名, 主色)。没命中就用域名兜底 + 默认主色。
/// 后续加站点只改这里（或再下钻到独立识别器）。
fn brand_for(url: &str) -> (String, String) {
    let host = host_of(url);
    let h = host.to_ascii_lowercase();
    if h.contains("baidu.com") {
        return ("百度".into(), "#2932E1".into());
    }
    if h.contains("wust.edu.cn") {
        return ("武汉科技大学".into(), "#8E1F2F".into());
    }
    if h.contains("github.com") {
        return ("GitHub".into(), "#24292F".into());
    }
    if h.contains("bilibili.com") {
        return ("哔哩哔哩".into(), "#FB7299".into());
    }
    // 兜底：取 host 最左段（去 www）
    let label = h
        .split('.')
        .next()
        .unwrap_or("")
        .trim_start_matches("www")
        .to_string();
    let name = if label.is_empty() { host } else { label };
    (name, "#1E6FFF".into())
}

fn host_of(url: &str) -> String {
    let origin = origin_of(url);
    origin
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .to_string()
}

fn scheme_of(url: &str) -> String {
    url.split("://").next().unwrap_or("https").to_string()
}

/// 候选 logo：扫前若干 <img>，命中 id="lg" 或 id/class/src/alt 含 "logo" 的绝对 URL。
fn find_logo(html: &str, base: &str) -> Option<String> {
    let mut from = 0usize;
    for _ in 0..60 {
        let s = match find_ci(html, "<img", from) {
            Some(x) => x,
            None => return None,
        };
        let open_len = tag_open_len(&html[s..]);
        if open_len == 0 {
            return None;
        }
        from = s + open_len + 1;
        let tag = &html[s..s + open_len + 1];
        let src = match attr_value(tag, "src") {
            Some(v) => v,
            None => continue,
        };
        let resolved = match resolve_abs(&src, base, &origin_of(base), &scheme_of(base)) {
            Some(r) => r,
            None => continue,
        };
        let id = attr_value(tag, "id").unwrap_or_default().to_ascii_lowercase();
        let class = attr_value(tag, "class").unwrap_or_default().to_ascii_lowercase();
        let alt = attr_value(tag, "alt").unwrap_or_default().to_ascii_lowercase();
        let src_l = resolved.to_ascii_lowercase();
        let strong = id == "lg" || id.contains("logo") || class.contains("logo")
            || alt.contains("logo") || src_l.contains("/logo") || src_l.contains("logo.");
        let weak = alt.contains("logo") || alt.contains("徽标") || class.contains("s-logo");
        if strong || weak {
            return Some(resolved);
        }
    }
    None
}

/// 搜索框：type=text/search / name=wd / id=kw 的输入框 → {placeholder, button}。
fn find_search(html: &str) -> Option<Value> {
    let mut from = 0usize;
    for _ in 0..30 {
        let s = match find_ci(html, "<input", from) {
            Some(x) => x,
            None => return None,
        };
        let open_len = tag_open_len(&html[s..]);
        if open_len == 0 {
            return None;
        }
        from = s + open_len + 1;
        let tag = &html[s..s + open_len + 1];
        let low = tag.to_ascii_lowercase();
        let ty = attr_value(tag, "type").unwrap_or_default().to_ascii_lowercase();
        let name = attr_value(tag, "name").unwrap_or_default().to_ascii_lowercase();
        let id = attr_value(tag, "id").unwrap_or_default().to_ascii_lowercase();
        if low.contains("submit") || ty == "hidden" || ty == "password" {
            continue;
        }
        let is_query = ty == "search" || ty == "text" || name == "wd" || name == "q" || id == "kw";
        if !is_query {
            continue;
        }
        let placeholder = attr_value(tag, "placeholder")
            .unwrap_or_default()
            .trim()
            .to_string();
        // 就近取提交按钮文字：往后 1200 字节内第一个 type=submit 的 value
        let window = &html[from..(from + 1200).min(html.len())];
        let mut button: String = String::new();
        let mut wf = 0usize;
        while let Some(b) = find_ci(window, "<input", wf) {
            let bl = tag_open_len(&window[b..]);
            if bl == 0 {
                break;
            }
            wf = b + bl + 1;
            let btag = &window[b..b + bl + 1];
            let bt = attr_value(btag, "type").unwrap_or_default().to_ascii_lowercase();
            if bt.contains("submit") {
                button = attr_value(btag, "value").unwrap_or_default().trim().to_string();
                break;
            }
        }
        if button.is_empty() {
            // 找 <button ...>文字</button>
            if let Some(b) = find_ci(window, "<button", 0) {
                let bl = tag_open_len(&window[b..]);
                if bl > 0 {
                    let inner = &window[b + bl + 1..];
                    if let Some(c) = find_ci(inner, "</button", 0) {
                        button = cap(collapse(&unescape(&plain_text(&inner[..c]))), 20);
                    }
                }
            }
        }
        if button.is_empty() {
            button = "搜索".into();
        }
        return Some(json!({ "placeholder": placeholder, "button": button }));
    }
    None
}

/// 导航（页首若干链接，去站内自链）+ 页脚（未进导航的尾段链接）。
fn split_nav_footer(links: &[Value], url: &str) -> (Vec<Value>, Vec<Value>) {
    let origin = origin_of(url);
    let mut nav: Vec<Value> = Vec::new();
    let mut footer: Vec<Value> = Vec::new();
    let total = links.len();
    for (i, l) in links.iter().enumerate() {
        let text = l.get("text").and_then(|v| v.as_str()).unwrap_or("").trim();
        let u = l.get("url").and_then(|v| v.as_str()).unwrap_or("");
        if text.is_empty() || u.is_empty() {
            continue;
        }
        // 站内首页自链不入导航
        if u.trim_end_matches('/') == origin.trim_end_matches('/') {
            continue;
        }
        // 顶栏导航剔除账号/杂项入口，贴近站点主导航
        if ["登录", "设置", "注册", "百度首页", "更多"].contains(&text) {
            continue;
        }
        let in_nav = nav.iter().any(|n| n == l);
        if i < 8 && nav.len() < 8 {
            nav.push(l.clone());
        } else if footer.len() < 5
            && !in_nav
            && (total - i <= 5 || text.contains('©') || text.contains("ICP") || text.contains("关于"))
        {
            footer.push(l.clone());
        }
    }
    (nav, footer)
}

// ---------------------------------------------------------------------------
// 轻量 HTML 提取（零依赖手写；目标是"够用的正文摘要"，不是完整解析器）
// ---------------------------------------------------------------------------

/// 去掉注释与 script/style/template/noscript 元素，保留其余结构供提取。
fn preprocess(html: &str) -> String {
    drop_elements(&strip_comments(html), &["script", "style", "template", "noscript"])
}

fn strip_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        match rest.find("<!--") {
            Some(i) => {
                out.push_str(&rest[..i]);
                let tail = &rest[i + 4..];
                rest = match tail.find("-->") {
                    Some(e) => &tail[e + 3..],
                    None => "",
                };
            }
            None => {
                out.push_str(rest);
                break;
            }
        }
    }
    out
}

/// 删除给定名字的**元素（连同其内容）**，其余标签与文本原样保留，供结构提取。
fn drop_elements(text: &str, names: &[&str]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    while i < text.len() {
        // 先把文本原样抄到下一个 '<'
        let s = match text[i..].find('<') {
            Some(x) => i + x,
            None => {
                out.push_str(&text[i..]);
                break;
            }
        };
        out.push_str(&text[i..s]);
        let tail = &text[s + 1..];
        if tail.starts_with('/') || tail.starts_with('!') || tail.starts_with('?') {
            // 结束标签 / 注释残渣 / 声明：原样保留后跳到 '>'
            let e = tail.find('>').map(|e| e + 1).unwrap_or(tail.len());
            out.push_str(&text[s..s + 1 + e]);
            i = s + 1 + e;
            if !tail.contains('>') {
                break;
            }
            continue;
        }
        // 读元素名
        let name_end = tail
            .find(|c: char| c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '>' || c == '/')
            .unwrap_or(tail.len());
        let name = tail[..name_end].to_ascii_lowercase();
        let tag_close = tag_open_len(&text[s..]); // 自 s 起到 '>' 的偏移
        if tag_close == 0 {
            // 没有闭合 '>'：正文残片，照抄并结束
            out.push_str(&text[s..]);
            break;
        }
        if names.contains(&name.as_str()) {
            // 丢弃该元素连同内容：跳到 </name> 之后
            let after = &text[s + tag_close + 1..];
            i = match find_ci(after, &format!("</{name}"), 0) {
                Some(c) => {
                    let closer = &after[c..];
                    let ce = closer.find('>').map(|e| e + 1).unwrap_or(closer.len());
                    s + tag_close + 1 + c + ce
                }
                None => text.len(),
            };
        } else {
            // 普通标签：原样保留，继续往后
            out.push_str(&text[s..s + tag_close + 1]);
            i = s + tag_close + 1;
        }
    }
    out
}

/// 返回从 `tag` 开头到 `>` 的偏移（'<' 之后算 0）；尊重引号内字符。
fn tag_open_len(s: &str) -> usize {
    let b = s.as_bytes();
    let mut i = 1usize; // 跳过 '<'
    let mut quote = 0u8;
    while i < b.len() {
        let c = b[i];
        if quote != 0 {
            if c == quote {
                quote = 0;
            }
        } else if c == b'"' || c == b'\'' {
            quote = c;
        } else if c == b'>' {
            return i;
        }
        i += 1;
    }
    0
}

/// ASCII 大小写不敏感子串查找（HTML 标签/属性名匹配够用）。
fn find_ci(hay: &str, pat: &str, from: usize) -> Option<usize> {
    if pat.is_empty() {
        return Some(from.min(hay.len()));
    }
    let hb = hay.as_bytes();
    let pb = pat.as_bytes();
    let mut i = from;
    while i + pb.len() <= hb.len() {
        let mut ok = true;
        for (j, &p) in pb.iter().enumerate() {
            if hb[i + j].to_ascii_lowercase() != p.to_ascii_lowercase() {
                ok = false;
                break;
            }
        }
        if ok {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn extract_title(text: &str) -> String {
    if let Some(s) = find_ci(text, "<title", 0) {
        let after = &text[s..];
        if let Some(gt) = after.find('>') {
            let inner = &after[gt + 1..];
            if let Some(c) = find_ci(inner, "</title", 0) {
                return inner[..c].to_string();
            }
        }
    }
    String::new()
}

/// 取首个 name/property 命中的 meta 的 content。
fn extract_meta(text: &str, names: &[&str]) -> String {
    let mut from = 0usize;
    while let Some(s) = find_ci(text, "<meta", from) {
        // 用 tag_open_len（尊重引号）切整块，避免属性值里的 `>` 截断
        let e = tag_open_len(&text[s..]);
        if e == 0 {
            break;
        }
        let block = &text[s..s + e];
        from = s + e + 1;
        let low = block.to_ascii_lowercase();
        let want = names
            .iter()
            .any(|n| low.contains(n) && (low.contains("name=") || low.contains("property=")));
        if !want {
            continue;
        }
        if let Some(v) = attr_value(block, "content") {
            return v;
        }
    }
    String::new()
}

/// 取标签属性值（key 大小写不敏感；值带引号/裸值均可）。`tag` 与 `low` 等长，
/// 用 ASCII lower 后的偏移直接切片原串（lower 不改变字节长度）。
fn attr_value(tag: &str, key: &str) -> Option<String> {
    let low = tag.to_ascii_lowercase();
    let bytes = low.as_bytes();
    let mut i = 0usize;
    while let Some(k) = find_ci(&low, key, i) {
        i = k + key.len();
        // 跳过空白直到 '='
        let mut j = i;
        while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t' || bytes[j] == b'\n' || bytes[j] == b'\r') {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b'=' {
            continue;
        }
        j += 1;
        while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t' || bytes[j] == b'\n' || bytes[j] == b'\r') {
            j += 1;
        }
        if j >= bytes.len() {
            return None;
        }
        let q = bytes[j];
        if q == b'"' || q == b'\'' {
            let close = (j + 1..bytes.len()).find(|&c| bytes[c] == q)?;
            return Some(tag[j + 1..close].to_string());
        }
        let mut e = j;
        while e < bytes.len()
            && bytes[e] != b' '
            && bytes[e] != b'\t'
            && bytes[e] != b'\n'
            && bytes[e] != b'\r'
            && bytes[e] != b'>'
        {
            e += 1;
        }
        return Some(tag[j..e].to_string());
    }
    None
}

/// 扫描锚点：绝对化 http(s) 链接 + 去重 + 文本精简。`base` 为最终落地 URL。
fn extract_links(html: &str, base: &str) -> Vec<Value> {
    let origin = origin_of(base);
    let scheme = base.split("://").next().unwrap_or("https").to_string();
    let mut out: Vec<Value> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let mut from = 0usize;
    while out.len() < MAX_LINKS {
        let s = match find_ci(html, "<a", from) {
            Some(x) => x,
            None => break,
        };
        let open_len = tag_open_len(&html[s..]);
        if open_len == 0 {
            from = s + 2;
            continue;
        }
        let tag = &html[s..s + open_len + 1];
        let href = match attr_value(tag, "href") {
            Some(h) => h,
            None => {
                from = s + open_len + 1;
                continue;
            }
        };
        let inner_start = s + open_len + 1;
        let after = &html[inner_start..];
        let inner_end = match find_ci(after, "</a", 0) {
            Some(x) => x,
            None => after.len(),
        };
        from = inner_start + inner_end + 4; // 越到本锚点之后，避免重复扫
        let resolved = match resolve_abs(&href, base, &origin, &scheme) {
            Some(r) => r,
            None => continue,
        };
        let text = cap(unescape(&strip_text(&html[inner_start..inner_start + inner_end])), 120);
        if text.is_empty() {
            continue;
        }
        if seen.iter().any(|s| s == &resolved) {
            continue;
        }
        seen.push(resolved.clone());
        out.push(json!({ "text": text, "url": resolved }));
    }
    out
}

/// 把 href 解析成绝对 http(s) URL；非 http(s)（javascript:/mailto:/相对路径）返回 None。
fn resolve_abs(href: &str, base: &str, origin: &str, scheme: &str) -> Option<String> {
    let h = href.trim();
    if h.is_empty() {
        return None;
    }
    if h.starts_with("http://") || h.starts_with("https://") {
        Some(h.to_string())
    } else if h.starts_with("//") {
        Some(format!("{scheme}:{h}"))
    } else if h.starts_with('/') {
        Some(format!("{origin}{h}"))
    } else {
        let _ = base;
        None
    }
}

/// `scheme://host[:port]`。`u[..idx+3]` 已含 `://`，直接拼 host 即可。
fn origin_of(u: &str) -> String {
    match u.find("://") {
        Some(idx) => {
            let rest = &u[idx + 3..];
            let end = rest
                .find(|c: char| c == '/' || c == '?' || c == '#')
                .unwrap_or(rest.len());
            format!("{}{}", &u[..idx + 3], &rest[..end])
        }
        None => u.to_string(),
    }
}

/// 去标签拿纯文本：去掉所有 `<...>`。
fn plain_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    loop {
        match rest.find('<') {
            Some(i) => {
                out.push_str(&rest[..i]);
                // 不注入空格：内联标签（<b>/<span>…）在浏览器里不产生空白；
                // 块间分隔靠源码本身已有的空白，由 collapse 统一收敛。
                let after = &rest[i + 1..];
                rest = match after.find('>') {
                    Some(e) => &after[e + 1..],
                    None => {
                        out.push_str(after);
                        break;
                    }
                };
            }
            None => {
                out.push_str(rest);
                break;
            }
        }
    }
    out
}

/// HTML 实体解码（常用命名 + 数字十进制/十六进制）。
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(a) = rest.find('&') {
        out.push_str(&rest[..a]);
        let tail = &rest[a + 1..];
        // 常见命名实体
        let mut hit = None;
        for (k, v) in [
            ("amp;", "&"),
            ("lt;", "<"),
            ("gt;", ">"),
            ("quot;", "\""),
            ("apos;", "'"),
            ("nbsp;", " "),
        ] {
            if tail.starts_with(k) {
                out.push_str(v);
                hit = Some(k.len());
                break;
            }
        }
        if let Some(klen) = hit {
            rest = &tail[klen..];
            continue;
        }
        // 数字实体 &#dd; / &#xhh;
        let (hex, digits) = if let Some(d) = tail.strip_prefix("#x") {
            (true, d)
        } else if let Some(d) = tail.strip_prefix('#') {
            (false, d)
        } else {
            (false, "")
        };
        if digits.is_empty() {
            out.push('&');
            rest = tail;
            continue;
        }
        let db = digits.as_bytes();
        let mut j = 0usize;
        while j < db.len()
            && (if hex {
                db[j].is_ascii_hexdigit()
            } else {
                db[j].is_ascii_digit()
            })
        {
            j += 1;
        }
        if j > 0 && digits[j..].starts_with(';') {
            if let Ok(v) = u32::from_str_radix(&digits[..j], if hex { 16 } else { 10 }) {
                out.push(char::from_u32(v).unwrap_or('\u{fffd}'));
                rest = &tail[(if hex { 2 } else { 1 }) + j + 1..];
                continue;
            }
        }
        out.push('&');
        rest = tail;
    }
    out.push_str(rest);
    out
}

fn strip_text(html: &str) -> String {
    collapse(&unescape(&plain_text(html)))
}

fn collapse(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut ws = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !ws && !out.is_empty() {
                out.push(' ');
            }
            ws = true;
        } else {
            out.push(c);
            ws = false;
        }
    }
    out.trim().to_string()
}

fn cap(s: String, n: usize) -> String {
    let chars: Vec<char> = s.chars().take(n).collect();
    let mut t: String = chars.into_iter().collect();
    if s.chars().count() > n {
        t.push('…');
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_title_meta_links() {
        let html = "<html><head><title>  测试站 &amp; 科技  </title>\
            <meta name=\"description\" content=\"  一个<b>测试</b>描述  \">\
            </head><body>\
            <nav><a href=\"https://a.test/x?y=1\">甲站<b>链接</b></a>\
            <a href=\"/rel\">相对站</a>\
            <a href=\"//cdn.test/z\">协议相对</a>\
            <a href=\"javascript:void(0)\" onclick=\"x\">脚本链接</a>\
            <a href=\"https://b.test/img\"><img src=\"i.png\" alt=\"\"></a></nav>\
            <script>var x=\"<a href='https://fake.test'>f</a>\";</script>\
            </body></html>";
        let pp = preprocess(html);
        let title = cap(unescape(&strip_text(&extract_title(&pp))), 200);
        let desc = cap(unescape(&strip_text(&extract_meta(&pp, &["description", "og:description"]))), 400);
        let links = extract_links(&pp, "https://a.test/");
        assert_eq!(title, "测试站 & 科技");
        assert_eq!(desc, "一个测试描述");
        // 脚本内假链接被剔除；javascript: 与空 alt 图片锚点被剔除
        let urls: Vec<&str> = links
            .iter()
            .map(|l| l.get("url").unwrap().as_str().unwrap())
            .collect();
        assert_eq!(
            urls,
            vec![
                "https://a.test/x?y=1",
                "https://a.test/rel",
                "https://cdn.test/z"
            ]
        );
        let texts: Vec<&str> = links
            .iter()
            .map(|l| l.get("text").unwrap().as_str().unwrap())
            .collect();
        assert_eq!(texts, vec!["甲站链接", "相对站", "协议相对"]);
    }

    #[test]
    fn unescape_numeric_and_named() {
        assert_eq!(unescape("a&amp;b&lt;c&gt;&quot;d&#39;e&#x27;f&nbsp;g"), "a&b<c>\"d'e'f g");
    }

    /// 实网探针：确认真页面能提出像样的标题/描述/链接。
    #[tokio::test]
    #[ignore = "hits the network"]
    async fn live_baidu() {
        let page = fetch_page("https://www.baidu.com").await.expect("fetch ok");
        let title = page.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let links = page.get("links").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
        println!("TITLE={title} LINKS={links} STATUS={}", page["status"]);
        assert!(title.contains("百度"), "title should be baidu, got {title:?}");
        assert!(links > 0, "expected some links");
        println!(
            "BRAND={}/{} LOGO={:?} NAV={} SEARCH={:?} FOOTER={}",
            page.get("brand_name").and_then(|v| v.as_str()).unwrap_or(""),
            page.get("brand_color").and_then(|v| v.as_str()).unwrap_or(""),
            page.get("logo"),
            page.get("nav").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0),
            page.get("search"),
            page.get("footer").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0),
        );
        assert_eq!(page.get("brand_name").and_then(|v| v.as_str()), Some("百度"));
        let nav = page.get("nav").and_then(|v| v.as_array()).unwrap();
        assert!(!nav.is_empty(), "expected nav links");
        assert!(!nav.iter().any(|l| l.get("text").and_then(|v| v.as_str()) == Some("登录")),
            "nav should not contain 登录");
        assert!(page.get("search").is_some(), "expected a search box");
        for l in nav.iter().take(8) {
            println!("  nav: {} → {}", l["text"], l["url"]);
        }
    }
}
