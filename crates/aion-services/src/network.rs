//! NetworkService：网络管理。目标白名单检查 + TCP / HTTP 封装。

use std::collections::HashMap;
use std::time::Duration;

use aion_adapter::AdapterKit;
use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::error::{AionError, AionResult};
use crate::security::SecurityContext;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// HTTP 响应。
#[derive(Debug, Clone)]
pub struct HttpReply {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl HttpReply {
    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// 粗略提取 HTML <title>。
    pub fn title(&self) -> Option<String> {
        let text = self.body_text();
        let start = text.find("<title")?;
        let after = &text[start..];
        let inner_start = after.find('>')? + 1;
        let rest = &after[inner_start..];
        let end = rest.find("</title>")?;
        Some(rest[..end].trim().to_string())
    }
}

/// 网络管理服务。
pub struct NetworkService {
    kit: AdapterKit,
}

impl NetworkService {
    pub fn new(kit: AdapterKit) -> Self {
        NetworkService { kit }
    }

    /// 出站 TCP 连接（目标必须在白名单内）。
    pub async fn tcp_connect(
        &self,
        sec: &SecurityContext,
        host: &str,
        port: u16,
    ) -> AionResult<TcpStream> {
        sec.check_cap("net:connect")?;
        sec.check_net(host, port)?;
        Ok(self.kit.net.tcp_connect(host, port, CONNECT_TIMEOUT).await?)
    }

    /// 监听端口。
    pub async fn tcp_bind(
        &self,
        sec: &SecurityContext,
        addr: &str,
    ) -> AionResult<TcpListener> {
        sec.check_cap("net:bind")?;
        Ok(self.kit.net.tcp_bind(addr).await?)
    }

    /// 抓取 HTTP 页面（http:// ；HTTPS 需要外接 TLS 栈，服务层不做）。
    pub async fn http_get(
        &self,
        sec: &SecurityContext,
        url: &str,
    ) -> AionResult<HttpReply> {
        let (host, port, path) = parse_url(url)?;
        let mut stream = self.tcp_connect(sec, &host, port).await?;

        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: AION/0.1\r\nAccept: */*\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await?;
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await?;

        parse_http_reply(&raw)
            .ok_or_else(|| AionError::Other(format!("malformed HTTP response from {host}")))
    }
}

/// 解析 `http://host[:port]/path`。
pub fn parse_url(url: &str) -> AionResult<(String, u16, String)> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| AionError::Other(format!("only http:// URLs are supported: {url}")))?;
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let hostport = hostport.trim_end_matches('/');
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (
            h.to_string(),
            p.parse::<u16>()
                .map_err(|_| AionError::Other(format!("invalid port in {url}")))?,
        ),
        None => (hostport.to_string(), 80),
    };
    if host.is_empty() {
        return Err(AionError::Other(format!("invalid URL: {url}")));
    }
    Ok((host, port, path.to_string()))
}

fn parse_http_reply(raw: &[u8]) -> Option<HttpReply> {
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let body = raw[split + 4..].to_vec();
    let mut lines = head.lines();
    let status_line = lines.next()?;
    let status = status_line.split_whitespace().nth(1)?.parse().ok()?;
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    Some(HttpReply {
        status,
        headers,
        body,
    })
}

#[async_trait]
impl cordis::Service for NetworkService {
    fn name(&self) -> &'static str {
        "network"
    }

    fn description(&self) -> &'static str {
        "网络管理"
    }

    async fn start(&self, ctx: &cordis::Context) -> cordis::CordisResult<()> {
        ctx.info("NetworkService ready");
        Ok(())
    }
}
