//! AION Desktop —— 原生全屏壳。
//!
//! 起一个无边框全屏窗口,内嵌 WebKit(webkit2gtk,经 wry)加载本机
//! AION web `http://127.0.0.1:18080`。对外没有浏览器外观/地址栏:
//! 屏幕上的唯一界面就是 AION 本身(输入框 + UIBlock 画布)。
//!
//! 与 "kiosk 浏览器" 的区别:渲染引擎被编进**我们自己的二进制**里,
//! 不存在一个独立的浏览器进程/应用。后续若把内部 WebKit 换成 egui/iced
//! 原生渲染,壳的职责边界不变——这就是通往 "AION 即桌面" 的壳层。

use wry::application::event::{Event, WindowEvent};
use wry::application::event_loop::{ControlFlow, EventLoop};
use wry::application::window::{Fullscreen, WindowBuilder};
use wry::webview::WebViewBuilder;

/// AION web 后端地址(与 web 同机;若换机器改这里)。
const AION_URL: &str = "http://127.0.0.1:18080";

fn main() -> wry::Result<()> {
    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("AION Desktop")
        .with_inner_size(wry::application::dpi::LogicalSize::new(1280.0, 800.0))
        .build(&event_loop)?;

    // 全屏无边框;matchbox-window-manager(会话脚本里已启动)会把单窗口铺满。
    let _ = window.set_fullscreen(Some(Fullscreen::Borderless(None)));

    // 内嵌 WebKit 加载 AION。WebViewBuilder::new 按值消费 window,须在全屏设置之后。
    let _webview = WebViewBuilder::new(window)?.with_url(AION_URL)?.build()?;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = event
        {
            *control_flow = ControlFlow::Exit;
        }
    });
}
