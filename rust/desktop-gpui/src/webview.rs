//! 窗口内嵌 Vditor 编辑器(WKWebView 宿主)。
//!
//! vditor 资源与接口由 bridge.rs 的 127.0.0.1 loopback 提供(不变),
//! 这里只负责把 WKWebView 挂进窗口 contentView(与登录框原生输入同套
//! 技术:contentView 子视图 + wantsLayer 防 Metal 面遮盖),按 canvas
//! prepaint 汇报的 bounds 定位。打开流程:open_vditor → load(url) 进
//! 待导航队列 → 下次 layout 消费并 loadRequest。

#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)] // objc 0.2 宏内部展开 #[cfg(cargo-clippy)],新 rustc 会告警
mod macos {
    // 链接 WebKit 框架(WKWebView)。#[link] 只对 extern 块生效,放模块上不 honored。
    #[link(name = "WebKit", kind = "framework")]
    extern "C" {}

    use std::ffi::{c_char, CStr, CString};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};

    use cocoa::base::id;
    use cocoa::foundation::{NSPoint, NSRect, NSSize};
    use gpui::{Bounds, Pixels, Window};
    use objc::declare::ClassDecl;
    use objc::runtime::{Class, Object, Sel, BOOL, NO, YES};
    use objc::{class, msg_send, sel, sel_impl};

    use crate::native::container as native_container;

    /// 全局唯一编辑器 webview(同时只开一个文档,与 editor.open_id 语义一致)。
    pub struct WebViewHost {
        webview: id,
        /// WKNavigationDelegate 是 weak 引用,自保留。
        _delegate: id,
    }
    // AppKit 对象恒定主线程访问;Arc 可能跨线程移动,故显式标注。
    unsafe impl Send for WebViewHost {}
    unsafe impl Sync for WebViewHost {}

    static HOST: Mutex<Option<Arc<WebViewHost>>> = Mutex::new(None);
    /// 待导航 URL(open_vditor 可能先于 webview 创建,先排队)。
    static PENDING_URL: Mutex<Option<CString>> = Mutex::new(None);
    static CREATION_FAILED: AtomicBool = AtomicBool::new(false);

    // ---- 导航 delegate:页面加载成败诊断(ATS 拦截会走 didFailProvisional) ----

    fn nav_delegate_class() -> &'static Class {
        static ONCE: OnceLock<&'static Class> = OnceLock::new();
        ONCE.get_or_init(|| {
            let mut decl = ClassDecl::new("UrlnoteWebViewDelegate", class!(NSObject)).unwrap();
            unsafe {
                decl.add_method(
                    sel!(webView:didFinishNavigation:),
                    did_finish as extern "C" fn(&Object, Sel, id, id),
                );
                decl.add_method(
                    sel!(webView:didFailProvisionalNavigation:withError:),
                    did_fail as extern "C" fn(&Object, Sel, id, id, id),
                );
                decl.add_method(
                    sel!(webView:didFailNavigation:withError:),
                    did_fail as extern "C" fn(&Object, Sel, id, id, id),
                );
            }
            decl.register()
        })
    }

    extern "C" fn did_finish(_this: &Object, _cmd: Sel, _wv: id, _nav: id) {
        eprintln!("[webview] didFinishNavigation(页面加载完成)");
    }

    extern "C" fn did_fail(_this: &Object, _cmd: Sel, _wv: id, _nav: id, error: id) {
        unsafe {
            let desc: id = msg_send![error, localizedDescription];
            if !desc.is_null() {
                let p: *const c_char = msg_send![desc, UTF8String];
                eprintln!("[webview] didFailNavigation: {}", CStr::from_ptr(p).to_string_lossy());
            } else {
                eprintln!("[webview] didFailNavigation(code={})", 0);
            }
        }
    }

    fn container(window: &mut Window) -> Option<id> {
        native_container(window)
    }

    fn pt(p: Pixels) -> f64 {
        f32::from(p) as f64
    }

    /// 入队导航请求(幂等:覆盖旧值)。open_vditor 调用。
    pub fn load(url: &str) -> bool {
        if CREATION_FAILED.load(Ordering::Relaxed) {
            return false;
        }
        let Ok(c) = CString::new(url) else { return false };
        *PENDING_URL.lock().unwrap() = Some(c);
        true
    }

    /// 惰性创建并挂接 webview(首帧 layout 时调用)。
    pub fn ensure(window: &mut Window) -> Option<Arc<WebViewHost>> {
        {
            let h = HOST.lock().unwrap();
            if h.is_some() {
                return h.clone();
            }
        }
        if CREATION_FAILED.load(Ordering::Relaxed) {
            return None;
        }
        let parent = container(window)?;
        unsafe {
            // 用 Class::get 而非 class! 宏:类缺失时优雅降级(class! 会直接 panic,
            // 而 webview 是可选功能,不该让整个应用崩掉)。
            let Some(cfg_cls) = objc::runtime::Class::get("WKWebViewConfiguration") else {
                eprintln!("[webview] WKWebViewConfiguration 类不存在(WebKit 未加载?)");
                CREATION_FAILED.store(true, Ordering::Relaxed);
                return None;
            };
            let Some(wv_cls) = objc::runtime::Class::get("WKWebView") else {
                eprintln!("[webview] WKWebView 类不存在(WebKit 未加载?)");
                CREATION_FAILED.store(true, Ordering::Relaxed);
                return None;
            };
            let config: id = msg_send![cfg_cls, alloc];
            let config: id = msg_send![config, init];
            let frame = NSRect::new(NSPoint::new(0., 0.), NSSize::new(0., 0.));
            let webview: id = msg_send![wv_cls, alloc];
            let webview: id = msg_send![webview, initWithFrame: frame configuration: config];
            if webview.is_null() {
                eprintln!("[webview] WKWebView 创建失败");
                CREATION_FAILED.store(true, Ordering::Relaxed);
                return None;
            }
            // 与登录框同理:防 GPUI Metal 面遮盖
            let _: () = msg_send![webview, setWantsLayer: YES];
            let _: () = msg_send![webview, setHidden: YES];
            let d: id = msg_send![nav_delegate_class(), alloc];
            let d: id = msg_send![d, init];
            let _: () = msg_send![webview, setNavigationDelegate: d];
            let _: () = msg_send![parent, addSubview: webview];
            eprintln!("[webview] created {:?} parent={:?}", webview, parent);
            let host = Arc::new(WebViewHost { webview, _delegate: d });
            *HOST.lock().unwrap() = Some(host.clone());
            Some(host)
        }
    }

    /// 定位并显示;顺带消费待导航 URL。
    pub fn layout(bounds: Bounds<Pixels>, window: &mut Window) {
        let Some(host) = ensure(window) else { return };
        let Some(parent) = container(window) else { return };
        unsafe {
            let content: NSRect = msg_send![parent, bounds];
            let frame = NSRect::new(
                NSPoint::new(
                    pt(bounds.origin.x),
                    content.size.height - pt(bounds.origin.y) - pt(bounds.size.height),
                ),
                NSSize::new(pt(bounds.size.width), pt(bounds.size.height)),
            );
            let _: () = msg_send![host.webview, setFrame: frame];
            let _: () = msg_send![host.webview, setHidden: NO];
        }
        let pending = PENDING_URL.lock().unwrap().take();
        if let Some(url) = pending {
            unsafe {
                let nsurl: id = msg_send![class!(NSURL), URLWithString: ns_str(&url)];
                if !nsurl.is_null() {
                    let req: id = msg_send![class!(NSURLRequest), requestWithURL: nsurl];
                    let _: () = msg_send![host.webview, loadRequest: req];
                    eprintln!("[webview] load {}", url.to_string_lossy());
                } else {
                    eprintln!("[webview] 非法 URL:{}", url.to_string_lossy());
                }
            }
        }
    }

    /// 断开编辑器:隐藏并停掉页面 JS/网络,清掉待导航。
    pub fn close_pane() {
        *PENDING_URL.lock().unwrap() = None;
        let h = HOST.lock().unwrap();
        if let Some(host) = h.as_ref() {
            unsafe {
                let _: () = msg_send![host.webview, setHidden: YES];
                let blank = ns_str(c"about:blank");
                let nsurl: id = msg_send![class!(NSURL), URLWithString: blank];
                let req: id = msg_send![class!(NSURLRequest), requestWithURL: nsurl];
                let _: () = msg_send![host.webview, loadRequest: req];
            }
        }
    }

    /// 仅隐藏(占位区不在当前画面时,每帧可能被调用)。
    pub fn hide() {
        let h = HOST.lock().unwrap();
        if let Some(host) = h.as_ref() {
            unsafe {
                let _: () = msg_send![host.webview, setHidden: YES];
            }
        }
    }

    unsafe fn ns_str(s: &CStr) -> id {
        let obj: id = msg_send![class!(NSString), alloc];
        msg_send![obj, initWithUTF8String: s.as_ptr() as *const c_char]
    }
}

#[cfg(target_os = "macos")]
pub use macos::{close_pane, ensure, hide, layout, load, WebViewHost};

/// 非 macOS 平台:无内嵌 webview,vditor 维持外部浏览器。
#[cfg(not(target_os = "macos"))]
#[allow(dead_code)]
mod stub {
    use gpui::{Bounds, Pixels, Window};

    pub struct WebViewHost;
    pub fn load(_: &str) -> bool {
        false
    }
    pub fn ensure(_: &mut Window) -> Option<std::sync::Arc<WebViewHost>> {
        None
    }
    pub fn layout(_: Bounds<Pixels>, _: &mut Window) {}
    pub fn close_pane() {}
    pub fn hide() {}
}

#[cfg(not(target_os = "macos"))]
pub use stub::{close_pane, ensure, hide, layout, load, WebViewHost};
