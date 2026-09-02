//! Net 适配器：socket / connect / bind 封装。

use std::time::Duration;

use async_trait::async_trait;
use tokio::net::{TcpListener, TcpStream, UdpSocket};

use crate::{AdapterError, AdapterResult};

/// Net 适配器 trait。
#[async_trait]
pub trait NetAdapter: Send + Sync {
    /// 建立出站 TCP 连接（带超时）。
    async fn tcp_connect(&self, host: &str, port: u16, timeout: Duration) -> AdapterResult<TcpStream>;

    /// 监听 TCP 端口。
    async fn tcp_bind(&self, addr: &str) -> AdapterResult<TcpListener>;

    /// 绑定 UDP 端口。
    async fn udp_bind(&self, addr: &str) -> AdapterResult<UdpSocket>;
}

/// 平台原生实现（tokio::net，即对 socket/connect/bind 系统调用的封装）。
pub struct NativeNetAdapter;

#[async_trait]
impl NetAdapter for NativeNetAdapter {
    async fn tcp_connect(&self, host: &str, port: u16, timeout: Duration) -> AdapterResult<TcpStream> {
        let addr = format!("{host}:{port}");
        let fut = tokio::net::TcpStream::connect(&addr);
        match tokio::time::timeout(timeout, fut).await {
            Ok(Ok(stream)) => {
                stream.set_nodelay(true).ok();
                Ok(stream)
            }
            Ok(Err(e)) => Err(AdapterError::Io(e)),
            Err(_) => Err(AdapterError::Other(format!(
                "connect {addr} timed out after {timeout:?}"
            ))),
        }
    }

    async fn tcp_bind(&self, addr: &str) -> AdapterResult<TcpListener> {
        Ok(TcpListener::bind(addr).await?)
    }

    async fn udp_bind(&self, addr: &str) -> AdapterResult<UdpSocket> {
        Ok(UdpSocket::bind(addr).await?)
    }
}
