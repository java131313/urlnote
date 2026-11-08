//! 登录框原生输入(NSTextField / NSSecureTextField)宿主。
//!
//! GPUI 窗口是自绘 Metal 视图,IME/聚焦/闪烁与原生有差距;这里把真正的
//! AppKit 文本框作为子视图挂进 contentView,由 TextField(native 模式)
//! 的 canvas prepaint 每帧汇报 GPUI 布局 bounds 来定位(GPUI 的 px 在
//! macOS 上 == AppKit point;contentView 原点左下,y 需翻转)。
//!
//! 文本双向同步:
//! - native → GPUI:delegate `controlTextDidChange:` 直写 FieldInner.text
//!   (登录提交走 `f.text()` 读取,无需 cx.notify);
//! - GPUI → native:prepaint 比对 stringValue 回推(仅 set_text 初始值等,
//!   不相等才 set,避免打断原生 IME)。

#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)] // objc 0.2 宏内部展开 #[cfg(cargo-clippy)],新 rustc 会告警
mod macos {
    use std::ffi::{c_char, CStr};
    use std::sync::{Arc, Mutex, OnceLock, Weak};

    use cocoa::base::id;
    use cocoa::foundation::{NSPoint, NSRect, NSSize};
    use gpui::{Bounds, Pixels, Window};
    use objc::declare::ClassDecl;
    use objc::runtime::{Class, Object, Sel, BOOL, NO, YES};
    use objc::{class, msg_send, sel, sel_impl};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    use crate::input::FieldInner;

    /// 原生框共享给 delegate 的上下文。
    pub struct NativeCtx {
        /// GPUI 侧文本状态(native → GPUI 同步时直写)。
        pub inner: Arc<Mutex<FieldInner>>,
        /// 回车提交(回调里没有 window/cx,由调用方经前台 executor 回 GPUI)。
        pub on_submit: Option<Arc<dyn Fn() + Send + Sync>>,
    }

    pub struct NativeField {
        control: id,
        /// NSTextField.delegate 是 weak 引用,这里自保留防悬垂(进程生命周期)。
        _delegate: id,
    }

    // AppKit 对象恒定主线程访问(所有 native::* 都在 GPUI 主线程调用);
    // Arc 可能跨线程移动,故显式标注。
    unsafe impl Send for NativeField {}
    unsafe impl Sync for NativeField {}

    impl NativeField {
        fn new(password: bool, font_size: f32, placeholder: &str, ctx: NativeCtx) -> Arc<Self> {
            unsafe {
                let cls = if password { class!(NSSecureTextField) } else { class!(NSTextField) };
                let control: id = msg_send![cls, alloc];
                let control: id = msg_send![control, initWithFrame: NSRect::new(
                    NSPoint::new(0., 0.),
                    NSSize::new(0., 0.),
                )];

                // 标准圆角边框输入框外观
                let _: () = msg_send![control, setBezelStyle: 1i64]; // NSRoundedBezelStyle
                let _: () = msg_send![control, setBordered: YES];
                let _: () = msg_send![control, setEditable: YES];
                let _: () = msg_send![control, setSelectable: YES];
                // 混合 Metal/AppKit 层级:GPUI 的 native_view 背衬是 CAMetalLayer,
                // 非 layer-backed 的兄弟视图内容会被 Metal 面盖过(只见边框、
                // 文本被遮住/看着极窄)。强制本视图进 layer 树,按视图序合成。
                let _: () = msg_send![control, setWantsLayer: YES];

                let ph: id = msg_send![class!(NSString), alloc];
                let ph: id =
                    msg_send![ph, initWithUTF8String: placeholder.as_ptr() as *const c_char];
                let _: () = msg_send![control, setPlaceholderString: ph];

                let font: id = msg_send![class!(NSFont), systemFontOfSize: font_size as f64];
                let _: () = msg_send![control, setFont: font];

                let d: id = msg_send![delegate_class(), alloc];
                let d: id = msg_send![d, init];
                (*d).set_ivar::<usize>("ctx", Box::into_raw(Box::new(ctx)) as usize);
                let _: () = msg_send![control, setDelegate: d];

                let _: () = msg_send![control, setHidden: YES];
                Arc::new(Self { control, _delegate: d })
            }
        }
    }

    // ---- delegate:文本变更回写 + 回车提交 ----

    fn delegate_class() -> &'static Class {
        static ONCE: OnceLock<&'static Class> = OnceLock::new();
        ONCE.get_or_init(|| {
            let mut decl = ClassDecl::new("UrlnoteNativeFieldDelegate", class!(NSObject)).unwrap();
            decl.add_ivar::<usize>("ctx");
            unsafe {
                decl.add_method(
                    sel!(controlTextDidChange:),
                    control_text_did_change as extern "C" fn(&Object, Sel, id),
                );
                decl.add_method(
                    sel!(control:textView:doCommandBySelector:),
                    control_do_command as extern "C" fn(&Object, Sel, id, id, Sel) -> BOOL,
                );
            }
            decl.register()
        })
    }

    fn ctx_of(this: &Object) -> Option<&'static NativeCtx> {
        let ptr = unsafe { *this.get_ivar::<usize>("ctx") };
        if ptr == 0 {
            None
        } else {
            // Box 由 delegate 独享,进程生命周期内不释放。
            Some(unsafe { &*(ptr as *const NativeCtx) })
        }
    }

    extern "C" fn control_text_did_change(this: &Object, _cmd: Sel, note: id) {
        unsafe {
            let obj: id = msg_send![note, object];
            let sv: id = msg_send![obj, stringValue];
            let text = if sv.is_null() {
                String::new()
            } else {
                let p: *const c_char = msg_send![sv, UTF8String];
                CStr::from_ptr(p).to_string_lossy().into_owned()
            };
            let Some(ctx) = ctx_of(this) else { return };
            let mut g = ctx.inner.lock().unwrap();
            if g.text != text {
                g.text = text;
                g.cursor = g.text.len();
                g.anchor = g.cursor;
                g.marked = None;
            }
        }
    }

    extern "C" fn control_do_command(
        this: &Object,
        _cmd: Sel,
        _control: id,
        _text_view: id,
        command: Sel,
    ) -> BOOL {
        if command == sel!(insertNewline:) {
            if let Some(ctx) = ctx_of(this) {
                if let Some(cb) = &ctx.on_submit {
                    cb();
                }
            }
            return YES;
        }
        NO
    }

    // ---- 注册表:按创建顺序串 Tab 键序,支持整批隐藏 ----

    static REGISTRY: Mutex<Vec<Weak<NativeField>>> = Mutex::new(Vec::new());

    fn ns_view(window: &Window) -> Option<id> {
        let h = HasWindowHandle::window_handle(window).ok()?;
        match h.as_raw() {
            RawWindowHandle::AppKit(k) => Some(k.ns_view.as_ptr() as id),
            _ => None,
        }
    }

    /// 承载原生输入框的容器:contentView。
    /// rwh 给的 ns_view 是内嵌 CAMetalLayer 的自绘视图 native_view;
    /// 子视图挂在它上面可能不合成/不响应,而它的父视图 contentView
    /// 是普通 NSView,后加的子视图恒在 native_view 之上,事件与显示都可靠。
    /// webview.rs 的 WKWebView 宿主也复用这个容器。
    pub(crate) fn container(window: &Window) -> Option<id> {
        let view = ns_view(window)?;
        if view.is_null() {
            return None;
        }
        let parent: id = unsafe { msg_send![view, superview] };
        if parent.is_null() {
            return None;
        }
        Some(parent)
    }

    /// 在窗口 contentView 上创建并挂接原生输入框。
    pub fn create(
        window: &mut Window,
        password: bool,
        font_size: f32,
        placeholder: &str,
        ctx: NativeCtx,
    ) -> Option<Arc<NativeField>> {
        let parent = container(window)?;
        let host = NativeField::new(password, font_size, placeholder, ctx);
        unsafe {
            let _: () = msg_send![parent, addSubview: host.control];
            eprintln!(
                "[native] created {:?} password={} parent={:?}",
                host.control, password, parent
            );
            let mut reg = REGISTRY.lock().unwrap();
            // Tab 顺序 = 创建顺序(登录框 server → user → pass)
            if let Some(prev) = reg.iter().rev().find_map(|w| w.upgrade()) {
                let _: () = msg_send![prev.control, setNextKeyView: host.control];
            }
            reg.push(Arc::downgrade(&host));
        }
        Some(host)
    }

    /// GPUI 布局 bounds → AppKit frame 并显示。
    pub fn layout(field: &NativeField, bounds: Bounds<Pixels>, window: &mut Window) {
        fn pt(p: Pixels) -> f64 {
            f32::from(p) as f64
        }
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
            let _: () = msg_send![field.control, setFrame: frame];
            let _: () = msg_send![field.control, setHidden: NO];
        }
    }

    /// GPUI 文本状态 → 原生框(仅不一致时 set,避免打断原生 IME)。
    pub fn push_text(field: &NativeField, text: &str) {
        unsafe {
            let cur: id = msg_send![field.control, stringValue];
            if !cur.is_null() {
                let p: *const c_char = msg_send![cur, UTF8String];
                if CStr::from_ptr(p).to_bytes() == text.as_bytes() {
                    return;
                }
            }
            let s: id = msg_send![class!(NSString), alloc];
            let s: id = msg_send![s, initWithUTF8String: text.as_ptr() as *const c_char];
            let _: () = msg_send![field.control, setStringValue: s];
        }
    }

    /// 离开登录屏时整批隐藏(主界面不该露出原生框)。
    pub fn hide_all() {
        let reg = REGISTRY.lock().unwrap();
        for w in reg.iter() {
            if let Some(f) = w.upgrade() {
                unsafe {
                    let _: () = msg_send![f.control, setHidden: YES];
                }
            }
        }
    }

    /// 聚焦第一个原生框(GPUI_AUTO_FOCUS 实验用)。
    pub fn focus_first(window: &mut Window) {
        let Some(view) = ns_view(window) else { return };
        unsafe {
            let win: id = msg_send![view, window];
            if win.is_null() {
                return;
            }
            let reg = REGISTRY.lock().unwrap();
            if let Some(f) = reg.iter().find_map(|w| w.upgrade()) {
                let _: () = msg_send![win, makeFirstResponder: f.control];
            }
        }
    }
}

#[cfg(target_os = "macos")]
pub use macos::{create, focus_first, hide_all, layout, push_text, NativeCtx, NativeField};
#[cfg(target_os = "macos")]
pub(crate) use macos::container;

/// 非 macOS 平台:原生模式退化为自绘输入框(空实现)。
#[cfg(not(target_os = "macos"))]
#[allow(dead_code)]
mod stub {
    use std::sync::{Arc, Mutex};

    use gpui::{Bounds, Pixels, Window};

    use crate::input::FieldInner;

    pub struct NativeCtx {
        pub inner: Arc<Mutex<FieldInner>>,
        pub on_submit: Option<Arc<dyn Fn() + Send + Sync>>,
    }

    #[derive(Clone)]
    pub struct NativeField;

    pub fn create(
        _: &mut Window,
        _: bool,
        _: f32,
        _: &str,
        _: NativeCtx,
    ) -> Option<Arc<NativeField>> {
        None
    }
    pub fn layout(_: &NativeField, _: Bounds<Pixels>, _: &mut Window) {}
    pub fn push_text(_: &NativeField, _: &str) {}
    pub fn hide_all() {}
    pub fn focus_first(_: &mut Window) {}
}

#[cfg(not(target_os = "macos"))]
pub use stub::{create, focus_first, hide_all, layout, push_text, NativeCtx, NativeField};
