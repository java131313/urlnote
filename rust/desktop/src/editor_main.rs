//! Vditor 编辑器子进程:一个 tao 窗口包一个 wry webview,页面与数据全部来自
//! 主程序的 127.0.0.1 loopback(见主程序 bridge),不访问任何远程资源。
//! 主程序以子进程方式拉起:`urlnote-editor --port N --token T --id ID --title X`

use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};

fn arg_value(args: &[String], key: &str) -> String {
    args.iter()
        .position(|a| a == key)
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_default()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let port = arg_value(&args, "--port");
    let token = arg_value(&args, "--token");
    let id = arg_value(&args, "--id");
    let title = arg_value(&args, "--title");
    let title = if title.is_empty() { "编辑正文".into() } else { title };

    let event_loop = EventLoop::new();
    let window = tao::window::WindowBuilder::new()
        .with_title(format!("📝 {title}"))
        .with_inner_size(tao::dpi::LogicalSize::new(1024.0, 780.0))
        .with_min_inner_size(tao::dpi::LogicalSize::new(640.0, 480.0))
        .build(&event_loop)?;

    let webview = wry::WebViewBuilder::new().with_url(&format!(
        "http://127.0.0.1:{port}/editor/page?id={id}&token={token}"
    ));
    let _webview = webview.build(&window)?;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        if let Event::WindowEvent { event: WindowEvent::CloseRequested, .. } = event {
            *control_flow = ControlFlow::Exit;
        }
    })
}
