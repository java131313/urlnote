//! urlnote 桌面版(GPUI)——主窗口根视图 + 各屏 UI。
//!
//! 屏幕:登录 / 主界面(便签 | 文件库 | 画图)。输入框是 input::TextField
//! 子视图,正文/标题等改动经 cx.observe 回流到 AppState;后台消息由
//! tick 线程 drain 进状态并同步到输入框。

mod bridge;
mod draw;
mod input;
mod s3;
mod state;

use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Application, Bounds, Context, Entity, IntoElement, MouseButton, Pixels,
    Point, Render, SharedString, Size, TitlebarOptions, Window, WindowBounds, WindowOptions,
    deferred, div, hsla, point, px, rgb,
};

use input::TextField;
use state::{normalize_base, AppState, Msg, OssDialog, Screen, View, DEFAULT_API};

use crate::draw::{DrawEd, DrawEl, DrawTool};

pub struct RootView {
    state: Entity<AppState>,
    server_input: Entity<TextField>,
    user_input: Entity<TextField>,
    pass_input: Entity<TextField>,
    search_input: Entity<TextField>,
    title_input: Entity<TextField>,
    tags_input: Entity<TextField>,
    content_input: Entity<TextField>,
    oss_content_input: Entity<TextField>,
    dialog_input: Entity<TextField>,
    name_input: Entity<TextField>,
    /// 后台消息接收端(从 AppState 取来;pump 先收消息,有消息才 update state)
    rx: std::sync::Arc<std::sync::Mutex<std::sync::mpsc::Receiver<Msg>>>,
    /// 用户活动标志:输入框 observer / 画布操作置位,pump 据此跑 tick
    need_tick: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl RootView {
    pub fn new(window: &mut Window, cx: &mut Context<RootView>) -> Self {
        let state = cx.new(|cx| AppState::new(cx));

        let server_input = cx.new(|cx| {
            TextField::with_opts(cx, false, false, false, px(14.0), "https://… 或 http://localhost:8080")
        });
        let user_input = cx.new(|cx| TextField::with_opts(cx, false, false, false, px(14.0), "用户名"));
        let pass_input = cx.new(|cx| {
            let mut f = TextField::with_opts(cx, false, true, false, px(14.0), "密码");
            f.placeholder = "密码".into();
            f
        });
        let search_input =
            cx.new(|cx| TextField::with_opts(cx, false, false, false, px(13.0), "全文搜索…"));
        let title_input = cx.new(|cx| TextField::with_opts(cx, false, false, false, px(16.0), "标题"));
        let tags_input =
            cx.new(|cx| TextField::with_opts(cx, false, false, false, px(13.0), "逗号分隔"));
        let content_input = cx.new(|cx| {
            let mut f = TextField::multiline(cx);
            f.mono = true;
            f
        });
        let oss_content_input = cx.new(|cx| {
            let mut f = TextField::multiline(cx);
            f.mono = true;
            f
        });
        let dialog_input = cx.new(|cx| TextField::new(cx));
        let name_input = cx.new(|cx| TextField::with_opts(cx, false, false, false, px(13.0), "名称"));

        let api_base = state.read_with(cx, |st, _| st.api_base.clone());
        server_input.update(cx, |f, cx| f.set_text(api_base, cx));

        let rx = state.update(cx, |st, _cx| st.rx.take()).expect("state rx");
        let rx = std::sync::Arc::new(std::sync::Mutex::new(rx));
        let need_tick = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let nt_title = need_tick.clone();
        let nt_tags = need_tick.clone();
        let nt_content = need_tick.clone();
        let nt_oss = need_tick.clone();
        let nt_name = need_tick.clone();
        let nt_search = need_tick.clone();
        fn flag(nt: &std::sync::Arc<std::sync::atomic::AtomicBool>) {
            nt.store(true, std::sync::atomic::Ordering::SeqCst);
        }

        // ---- 观察者:输入框 → 状态 ----
        let st_e = state.clone();
        cx.observe(&title_input, move |root, field, cx| {
            let t = field.read(cx).text();
            root.state.update(cx, |st, cx| {
                if let Some(ed) = &mut st.current {
                    if t != ed.note.title {
                        ed.note.title = t;
                        ed.mark_edit();
                        flag(&nt_title);
                        cx.notify();
                    }
                }
            });
        })
        .detach();

        let st_e2 = state.clone();
        cx.observe(&tags_input, move |root, field, cx| {
            let t = field.read(cx).text();
            root.state.update(cx, |st, cx| {
                if let Some(ed) = &mut st.current {
                    let tags: Vec<String> = t
                        .split([',', '，'])
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                    if tags != ed.note.tags {
                        ed.note.tags = tags;
                        ed.mark_edit();
                        flag(&nt_tags);
                        cx.notify();
                    }
                }
            });
        })
        .detach();

        let st_e3 = state.clone();
        cx.observe(&content_input, move |root, field, cx| {
            let t = field.read(cx).text();
            root.state.update(cx, |st, cx| {
                if let Some(ed) = &mut st.current {
                    if t != ed.note.content {
                        ed.note.content = t;
                        ed.mark_edit();
                        flag(&nt_content);
                        cx.notify();
                    }
                }
            });
        })
        .detach();

        let st_e4 = state.clone();
        cx.observe(&oss_content_input, move |root, field, cx| {
            let t = field.read(cx).text();
            root.state.update(cx, |st, cx| {
                if let Some(f) = &mut st.oss.current {
                    if t != f.content {
                        f.content = t;
                        f.mark_edit();
                        flag(&nt_oss);
                        cx.notify();
                    }
                }
            });
        })
        .detach();

        let st_e5 = state.clone();
        cx.observe(&name_input, move |root, field, cx| {
            let t = field.read(cx).text();
            root.state.update(cx, |st, cx| {
                if let Some(ed) = &mut st.draw.ed {
                    if t != ed.name {
                        ed.name = t;
                        ed.mark_edit();
                        flag(&nt_name);
                        cx.notify();
                    }
                }
            });
        })
        .detach();

        let st_e6 = state.clone();
        cx.observe(&search_input, move |root, _field, cx| {
            flag(&nt_search);
            root.state.update(cx, |st, _cx| {
                st.search_dirty_at = Some(std::time::Instant::now());
            });
        })
        .detach();

        // 密码框回车 → 登录
        {
            let state = state.clone();
            let server = server_input.clone();
            let user = user_input.clone();
            pass_input.update(cx, |f, _cx| {
                f.on_enter = Some(Arc::new(move |_window, cx| {
                    let server_text = server.read_with(cx, |f, _| f.text());
                    let user_text = user.read_with(cx, |f, _| f.text());
                    let pass_text = cx.entity().read_with(cx, |f, _| f.text());
                    state.update(cx, |st, cx| {
                        let base = normalize_base(&server_text);
                        st.api_base = if base.is_empty() { DEFAULT_API.to_string() } else { base };
                        st.do_login(user_text, pass_text);
                        cx.notify();
                    });
                }));
            });
        }

        // 内容框特殊键 → 词典导航
        {
            let state = state.clone();
            let content = content_input.clone();
            content_input.update(cx, |f, _cx| {
                f.on_special = Some(Arc::new(move |key, _window, cx| {
                    let visible = state
                        .read_with(cx, |st, _| st.dict.visible() && st.current.is_some());
                    match key {
                        "up" | "down" => {
                            if visible {
                                state
                                    .update(cx, |st, cx| {
                                        let n = st.dict.words.len();
                                        if n > 0 {
                                            if key == "down" {
                                                st.dict.sel = (st.dict.sel + 1).min(n - 1);
                                            } else {
                                                st.dict.sel = st.dict.sel.saturating_sub(1);
                                            }
                                        }
                                        cx.notify();
                                    })
                                    ;
                            } else {
                                let up = key == "up";
                                content
                                    .update(cx, |f, cx| f.arrow_row(up, false, cx))
                                    ;
                            }
                        }
                        "enter" | "tab" => {
                            if visible {
                                let word = state
                                    .read_with(cx, |st, _| {
                                        st.dict.words.get(st.dict.sel).map(|w| w.word.clone())
                                    });
                                if let Some(w) = word {
                                    content.update(cx, |f, cx| f.accept_word(&w, cx));
                                }
                                state
                                    .update(cx, |st, cx| {
                                        st.dict.close();
                                        cx.notify();
                                    })
                                    ;
                            }
                        }
                        "escape" => {
                            if visible {
                                state
                                    .update(cx, |st, cx| {
                                        st.dict.close();
                                        cx.notify();
                                    })
                                    ;
                            }
                        }
                        _ => {}
                    }
                }));
            });
        }

        // ---- Vditor 桥 ----
        {
            let (tx, client, doc, api_base) = state.read_with(cx, |st, _| {
                (st.tx.clone(), st.client.clone(), st.editor.doc.clone(), st.editor.api_base.clone())
            });
            if let Some((port, token)) = bridge::start_bridge(tx, client, doc, api_base) {
                state.update(cx, |st, _| {
                    st.editor.port = port;
                    st.editor.token = token;
                });
            }
        }

        // ---- 启动 whoami ----
        {
            let base = state.read_with(cx, |st, _| st.api_base.clone());
            state.update(cx, |st, cx| {
                st.busy = true;
                st.go(move |c| {
                    let r = c.get(format!("{base}/api/whoami")).send();
                    Msg::Whoami(match r {
                        Ok(resp) => {
                            if resp.status().is_success() {
                                match resp.json::<shared::WhoamiResponse>() {
                                    Ok(w) => Ok(w.user.username),
                                    Err(e) => Err(state::err(e)),
                                }
                            } else {
                                Err("unauthorized".into())
                            }
                        }
                        Err(e) => Err(state::err(e)),
                    })
                });
                cx.notify();
            });
        }

        // ---- 前台 pump 协程:消息驱动 + 按需 tick ----
        // 发现:gpui 0.2 里对实体 update 本身就会使渲染过它的视图重绘,
        // 因此空闲时绝不动 AppState —— 否则周期性 update 造成 ~2Hz 持续重绘,
        // canvas 自绘文字每次重绘都重新光栅化 → "一闪一闪"。
        {
            let pump_rx = rx.clone();
            let pump_need = need_tick.clone();
            let search_e = search_input.clone();
            let content_e = content_input.clone();
            let title_e = title_input.clone();
            let tags_e = tags_input.clone();
            let oss_content_e = oss_content_input.clone();
            cx.spawn(async move |this, mut cx| {
                use std::sync::atomic::Ordering::SeqCst;
                // 上一轮 update 时是否仍有未完成的防抖/保存工作
                let mut was_active = false;
                loop {
                    enum Act {
                        Quit,
                        Sleep(u64),
                    }
                    // 1) 先收消息(不碰 state)
                    let mut msgs: Vec<Msg> = Vec::new();
                    {
                        let rx = pump_rx.lock().unwrap();
                        while let Ok(m) = rx.try_recv() {
                            msgs.push(m);
                        }
                    }
                    // 2) 是否有活动:observer 标志 / 上一轮遗留工作 / 有消息
                    let active = pump_need.swap(false, SeqCst) || was_active;
                    if msgs.is_empty() && !active {
                        let _ = gpui::Timer::after(Duration::from_millis(120)).await;
                        continue;
                    }
                    let had_msgs = !msgs.is_empty();
                    let act = cx
                        .update(|cx| {
                            let Some(root_ent) = this.upgrade() else {
                                return Act::Quit;
                            };
                            let mut still_active = false;
                            root_ent.update(cx, |root, cx| {
                                let search_text = search_e.read_with(cx, |f, _| f.text());
                                let (content_text, cursor) =
                                    content_e.read_with(cx, |f, _| (f.text(), f.cursor()));
                                let (note_sync, note_content, oss_sync) = root.state.update(
                                    cx,
                                    |st, cx| {
                                        for m in msgs {
                                            st.apply(m);
                                        }
                                        if st.tick(search_text, content_text, cursor) {
                                            pump_need.store(true, SeqCst);
                                        }
                                        (
                                            st.pending_note_sync.take(),
                                            st.pending_note_content.take(),
                                            st.pending_oss_sync.take(),
                                        )
                                    },
                                );
                                if let Some((id, title, tags, content, fs)) = note_sync {
                                    let cur_id = root.state.read_with(cx, |st, _| {
                                        st.current.as_ref().map(|e| e.note.id.clone())
                                    });
                                    if cur_id.as_deref() == Some(id.as_str()) {
                                        root.title_input.update(cx, |f, cx| {
                                            f.set_text(title, cx)
                                        });
                                        root.tags_input.update(cx, |f, cx| f.set_text(tags, cx));
                                        root.content_input.update(cx, |f, cx| {
                                            f.set_text(content, cx);
                                            f.set_font_size(px(fs.max(10) as f32), cx);
                                        });
                                    }
                                }
                                if let Some(content) = note_content {
                                    let (open_id, cur_note, cur_oss) = root.state.read_with(
                                        cx,
                                        |st, _| {
                                            (
                                                st.editor.open_id.clone(),
                                                st.current.as_ref().map(|e| e.note.id.clone()),
                                                st.oss.current.as_ref().map(|f| f.key.clone()),
                                            )
                                        },
                                    );
                                    if cur_note.as_deref() == Some(open_id.as_str()) {
                                        root.content_input.update(cx, |f, cx| {
                                            f.set_text(content.clone(), cx)
                                        });
                                    } else if cur_oss.as_deref() == Some(open_id.as_str()) {
                                        root.oss_content_input.update(cx, |f, cx| {
                                            f.set_text(content, cx)
                                        });
                                    }
                                }
                                if let Some((key, content, fs)) = oss_sync {
                                    let cur_key = root.state.read_with(cx, |st, _| {
                                        st.oss.current.as_ref().map(|f| f.key.clone())
                                    });
                                    if cur_key.as_deref() == Some(key.as_str()) {
                                        root.oss_content_input.update(cx, |f, cx| {
                                            f.set_text(content, cx);
                                            f.set_font_size(px(fs.max(10) as f32), cx);
                                        });
                                    }
                                }
                                still_active = root
                                    .state
                                    .read_with(cx, |st, _| st.pending_work());
                            });
                            // 消息驱动(UI 无输入事件),需在同一 update 内排入刷新效果,
                            // finish_update 时才会真正执行(AsyncApp::refresh 在 update 外
                            // 只排队不执行,见 gpui 0.2 Effect::RefreshWindows)。
                            cx.refresh_windows();
                            was_active = still_active;
                            Act::Sleep(if still_active { 150 } else { 120 })
                        })
                        .unwrap_or(Act::Quit);
                    match act {
                        Act::Quit => break,
                        Act::Sleep(ms) => {
                            // 本轮有实际更新(state/输入框):跨线程消息不会触发窗口失效,
                            // 依赖 refresh 强制重绘;空闲轮(ms==120 且无更新)不刷。
                            if std::env::var("GPUI_RENDER_LOG").is_ok() && had_msgs {
                                eprintln!("[pump] applied msgs, refresh queued");
                            }
                            let _ = gpui::Timer::after(Duration::from_millis(ms)).await;
                        }
                    }
                }
            })
            .detach();
        }

        let _ = st_e;
        let _ = st_e2;
        let _ = st_e3;
        let _ = st_e4;
        let _ = st_e5;
        let _ = st_e6;
        let _ = window;

        Self {
            state,
            server_input,
            user_input,
            pass_input,
            search_input,
            title_input,
            tags_input,
            content_input,
            oss_content_input,
            dialog_input,
            name_input,
            rx,
            need_tick,
        }
    }

    // ---------- 小部件 ----------

    fn btn(label: impl Into<SharedString>) -> gpui::Stateful<gpui::Div> {
        let label = label.into();
        div()
            .id(SharedString::from(format!("btn-{label}")))
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(0xe4e4e7))
            .text_color(rgb(0x27272a))
            .text_sm()
            .cursor(gpui::CursorStyle::PointingHand)
            .hover(|s| s.bg(rgb(0xd4d4d8)))
            .child(label)
    }

    fn primary_btn(label: impl Into<SharedString>) -> gpui::Stateful<gpui::Div> {
        let label = label.into();
        div()
            .id(SharedString::from(format!("pbtn-{label}")))
            .px_3()
            .py_1()
            .rounded_md()
            .bg(rgb(0x4f46e5))
            .text_color(rgb(0xffffff))
            .text_sm()
            .cursor(gpui::CursorStyle::PointingHand)
            .hover(|s| s.bg(rgb(0x4338ca)))
            .child(label)
    }

    fn tab(active: bool, label: &str) -> gpui::Stateful<gpui::Div> {
        div()
            .id(SharedString::from(format!("tab-{label}")))
            .px_3()
            .py_1()
            .rounded_md()
            .text_sm()
            .cursor(gpui::CursorStyle::PointingHand)
            .when(active, |d| d.bg(rgb(0x2b3a67)).text_color(rgb(0xffffff)))
            .when(!active, |d| d.text_color(rgb(0x3f3f46)).hover(|s| s.bg(rgb(0xe4e4e7))))
            .child(label.to_string())
    }

    fn overlay(panel: impl IntoElement) -> gpui::Div {
        div()
            .absolute()
            .inset_0()
            .bg(hsla(0.0, 0.0, 0.0, 0.35))
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .bg(rgb(0xffffff))
                    .rounded_lg()
                    .p_4()
                    .w(px(380.0))
                    .shadow_lg()
                    .child(panel),
            )
    }

    // ---------- 登录 ----------

    fn render_login(&self, busy: bool, login_err: &str) -> AnyElement {
        let state = self.state.clone();
        let server = self.server_input.clone();
        let user = self.user_input.clone();
        let pass = self.pass_input.clone();

        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgb(0xfafafa))
            .child(
                div()
                    .w(px(360.0))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(rgb(0x1e1b4b))
                            .child("📝 urlnote"),
                    )
                    .child(div().text_sm().text_color(rgb(0x71717a)).child("服务器地址"))
                    .child(self.server_input.clone())
                    .child(div().text_sm().text_color(rgb(0x71717a)).child("用户名"))
                    .child(self.user_input.clone())
                    .child(div().text_sm().text_color(rgb(0x71717a)).child("密码"))
                    .child(self.pass_input.clone())
                    .child(div().h(px(4.0)))
                    .child(if busy {
                        div().text_sm().text_color(rgb(0x71717a)).child("连接中…").into_any_element()
                    } else {
                        Self::primary_btn("登 录")
                            .on_click(move |_e, _window, cx| {
                                let server_text = server.read_with(cx, |f, _| f.text());
                                let user_text = user.read_with(cx, |f, _| f.text());
                                let pass_text = pass.read_with(cx, |f, _| f.text());
                                state.update(cx, |st, cx| {
                                    let base = normalize_base(&server_text);
                                    st.api_base =
                                        if base.is_empty() { DEFAULT_API.to_string() } else { base };
                                    st.do_login(user_text, pass_text);
                                    cx.notify();
                                });
                            })
                            .into_any_element()
                    })
                    .when(!login_err.is_empty(), |d| {
                        d.child(
                            div()
                                .text_sm()
                                .text_color(rgb(0xdc2626))
                                .child(login_err.to_string()),
                        )
                    })
                    .into_any_element(),
            )
            .into_any_element()
    }

    // ---------- 主工具栏 ----------

    fn render_toolbar(&self, st: &AppState) -> AnyElement {
        let state = self.state.clone();
        let state2 = self.state.clone();
        let state3 = self.state.clone();
        let state4 = self.state.clone();
        let title_e = self.title_input.clone();
        let tags_e = self.tags_input.clone();
        let content_e = self.content_input.clone();

        let mut bar = div()
            .h(px(44.0))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .bg(rgb(0xf4f4f5))
            .border_b_1()
            .border_color(rgb(0xe4e4e7))
            .child(
                div()
                    .text_base()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(rgb(0x1e1b4b))
                    .child("📝 urlnote"),
            )
            .child(Self::tab(st.view == View::Notes, "便签").on_click(move |_e, _w, cx| {
                state.update(cx, |st, cx| {
                    st.view = View::Notes;
                    cx.notify();
                });
            }))
            .child(Self::tab(st.view == View::Oss, "文件库").on_click(move |_e, _w, cx| {
                state2.update(cx, |st, cx| {
                    st.enter_oss();
                    cx.notify();
                });
            }))
            .child(Self::tab(st.view == View::Draw, "画图").on_click(move |_e, _w, cx| {
                state3.update(cx, |st, cx| {
                    st.enter_draw();
                    cx.notify();
                });
            }));

        if st.view == View::Notes {
            bar = bar
                .child(div().w(px(8.0)))
                .child(
                    Self::btn("＋ 新建").on_click(move |_e, _window, cx| {
                        content_e.update(cx, |f, cx| f.set_text(String::new(), cx));
                        title_e.update(cx, |f, cx| f.set_text(String::new(), cx));
                        tags_e.update(cx, |f, cx| f.set_text(String::new(), cx));
                        state4.update(cx, |st, cx| {
                            st.new_note();
                            cx.notify();
                        });
                    }),
                )
                .child(div().w(px(180.0)).child(self.search_input.clone()));
        }

        let state5 = self.state.clone();
        bar.child(
            div()
                .flex_1()
                .flex()
                .flex_row_reverse()
                .items_center()
                .gap_3()
                .child(Self::btn("退出登录").on_click(move |_e, _w, cx| {
                    state5.update(cx, |st, cx| {
                        st.do_logout();
                        cx.notify();
                    });
                }))
                .when(st.view == View::Notes && !st.status.is_empty(), |d| {
                    d.child(div().text_xs().text_color(rgb(0x71717a)).child(st.status.clone()))
                }),
        )
        .into_any_element()
    }

    // ---------- 便签 ----------

    fn render_notes(&self, st: &AppState) -> AnyElement {
        let state = self.state.clone();
        let current_id = st.current.as_ref().map(|e| e.note.id.clone());

        // 列表(uniform_list 自带滚动)
        let notes_data: Vec<(String, String, String, bool)> = st
            .notes
            .iter()
            .map(|meta| {
                let selected = current_id.as_deref() == Some(meta.id.as_str());
                let title = if meta.title.trim().is_empty() {
                    meta.excerpt.chars().take(24).collect::<String>()
                } else {
                    meta.title.clone()
                };
                let time =
                    meta.updated_at.trim_end_matches('Z').trim_end_matches(":00").to_string();
                (meta.id.clone(), title, time, selected)
            })
            .collect();
        let st_list = state.clone();
        let list = gpui::uniform_list(
            SharedString::from("notes-list"),
            notes_data.len(),
            move |range, _window, _cx| {
                let mut rows: Vec<AnyElement> = Vec::new();
                for i in range {
                    let (id, title, time, selected) = notes_data[i].clone();
                    let st_e = st_list.clone();
                    rows.push(
                        div()
                            .id(SharedString::from(format!("note-{id}")))
                            .px_2()
                            .py_2()
                            .mx_1()
                            .rounded_md()
                            .cursor(gpui::CursorStyle::PointingHand)
                            .when(selected, |d| d.bg(rgb(0xe0e7ff)))
                            .when(!selected, |d| d.hover(|s| s.bg(rgb(0xf4f4f5))))
                            .child(div().text_sm().text_color(rgb(0x27272a)).child(title))
                            .child(div().text_xs().text_color(rgb(0xa1a1aa)).child(time))
                            .on_click(move |_e, _w, cx| {
                                st_e.update(cx, |st, cx| {
                                    st.load_note(&id);
                                    cx.notify();
                                });
                            })
                            .into_any_element(),
                    );
                }
                rows
            },
        );

        let sidebar = div()
            .w(px(250.0))
            .h_full()
            .border_r_1()
            .border_color(rgb(0xe4e4e7))
            .bg(rgb(0xfbfcfe))
            .py_1()
            .child(list);

        // 编辑器区
        let ed_snap = st.current.as_ref().map(|ed| {
            (
                ed.note.id.clone(),
                ed.note.font_size,
                ed.dirty,
                ed.saving,
            )
        });

        let editor_open = !st.editor.open_id.is_empty();
        let editor_for_this =
            editor_open && current_id.as_deref() == Some(st.editor.open_id.as_str());
        let editor_other = editor_open && !editor_for_this;

        let state_v = state.clone();
        let state_disc = state.clone();

        let vditor_row: AnyElement = if editor_for_this {
            div()
                .h(px(36.0))
                .px_3()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(0x16a34a))
                        .child("✅ Vditor 编辑中(浏览器窗口,自动回写)"),
                )
                .child(Self::btn("✕ 断开编辑器").on_click(move |_e, _w, cx| {
                    state_disc.update(cx, |st, cx| {
                        st.close_editor();
                        cx.notify();
                    });
                }))
                .into_any_element()
        } else {
            div()
                .h(px(36.0))
                .px_3()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    Self::btn("📝 用 Vditor 编辑正文").on_click(move |_e, _w, cx| {
                        let id = state_v
                            .read_with(cx, |st, _| {
                                st.current.as_ref().map(|e| {
                                    (
                                        e.note.id.clone(),
                                        if e.note.title.trim().is_empty() {
                                            e.note.id.clone()
                                        } else {
                                            e.note.title.clone()
                                        },
                                        e.note.content.clone(),
                                    )
                                })
                            })
                            ;
                        if let Some((id, title, content)) = id {
                            state_v.update(cx, |st, cx| {
                                st.open_vditor(id, title, content);
                                cx.notify();
                            });
                        }
                    }),
                )
                .when(editor_other, |d| {
                    d.child(
                        div()
                            .text_sm()
                            .text_color(rgb(0xd97706))
                            .child("⚠️ 编辑器开着别的便签"),
                    )
                })
                .into_any_element()
        };

        let state_fs1 = state.clone();
        let state_fs2 = state.clone();
        let content_fs = self.content_input.clone();

        let mut editor_col = div()
            .flex_1()
            .h_full()
            .flex()
            .flex_col()
            .child(vditor_row)
            .border_color(rgb(0xe4e4e7))
            .child(
                div()
                    .px_3()
                    .pt_2()
                    .child(self.title_input.clone()),
            )
            .child(
                div()
                    .h(px(36.0))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().text_xs().text_color(rgb(0x71717a)).child("标签"))
                    .child(div().w(px(220.0)).child(self.tags_input.clone()))
                    .child(
                        Self::btn("A−").on_click(move |_e, _w, cx| {
                            content_fs.update(cx, |f, cx| {
                                let cur = f.inner.lock().unwrap().font_size;
                                f.set_font_size((cur - px(1.0)).max(px(10.0)), cx);
                            });
                            state_fs1.update(cx, |st, cx| {
                                if let Some(ed) = &mut st.current {
                                    ed.note.font_size =
                                        (ed.note.font_size - 1).max(shared::MIN_FONT_SIZE);
                                    ed.mark_edit();
                                }
                                cx.notify();
                            });
                        }),
                    )
                    .child(
                        Self::btn("A+").on_click(move |_e, _w, cx| {
                            state_fs2.update(cx, |st, cx| {
                                if let Some(ed) = &mut st.current {
                                    ed.note.font_size =
                                        (ed.note.font_size + 1).min(shared::MAX_FONT_SIZE);
                                    ed.mark_edit();
                                }
                                cx.notify();
                            });
                        }),
                    )
            );

        if let Some((_, font_size, dirty, saving)) = ed_snap {
            editor_col = editor_col.child(
                div()
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().text_xs().text_color(rgb(0x71717a)).child(format!("{font_size}px")))
                    .child(if dirty {
                        div().text_xs().text_color(rgb(0xd97706)).child("● 未保存").into_any_element()
                    } else if saving {
                        div().text_xs().text_color(rgb(0x71717a)).child("保存中…").into_any_element()
                    } else {
                        div().text_xs().text_color(rgb(0x71717a)).child("").into_any_element()
                    }),
            );
        }

        editor_col = editor_col
            .child(div().mx_3().h(px(1.0)).bg(rgb(0xe4e4e7)))
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.0))
                    .p_3()
                    .child(div().h_full().child(self.content_input.clone())),
            );

        // 词典面板
        if st.dict.visible() {
            let state_d = state.clone();
            let content_d = self.content_input.clone();
            let langs = st.dict.langs.clone();
            let cur_lang = st.dict.lang.clone();
            let words = st.dict.words.clone();
            let sel = st.dict.sel;
            let loading = st.dict.loading;

            let lang_label = langs
                .iter()
                .find(|l| l.code == cur_lang)
                .map(|l| format!("{} {}", l.code, l.name))
                .unwrap_or_else(|| cur_lang.clone());
            let next_lang = {
                let idx = langs.iter().position(|l| l.code == cur_lang);
                match idx {
                    Some(i) if !langs.is_empty() => langs[(i + 1) % langs.len()].code.clone(),
                    _ => String::new(),
                }
            };
            let mut dict_row: Vec<AnyElement> = vec![
                div().text_xs().text_color(rgb(0x71717a)).child("词典").into_any_element(),
                Self::btn(lang_label)
                    .on_click(move |_e, _w, cx| {
                        state_d.update(cx, |st, cx| {
                            st.dict.lang = next_lang.clone();
                            st.dict.close();
                            cx.notify();
                        });
                    })
                    .into_any_element(),
            ];
            if loading {
                dict_row.push(
                    div().text_xs().text_color(rgb(0x71717a)).child("查询中…").into_any_element(),
                );
            }
            for (i, w) in words.iter().enumerate() {
                let trans: Vec<String> = w.trans.iter().map(|(k, v)| format!("{k}:{v}")).collect();
                let label = if trans.is_empty() {
                    w.word.clone()
                } else {
                    format!("{}  ({})", w.word, trans.join("  "))
                };
                let state_w = state.clone();
                let content_w = content_d.clone();
                let word = w.word.clone();
                dict_row.push(
                    div()
                        .id(SharedString::from(format!("dict-chip-{i}")))
                        .px_2()
                        .py_0p5()
                        .rounded_full()
                        .text_xs()
                        .cursor(gpui::CursorStyle::PointingHand)
                        .when(i == sel, |d| d.bg(rgb(0x4f46e5)).text_color(rgb(0xffffff)))
                        .when(i != sel, |d| {
                            d.bg(rgb(0xf4f4f5)).text_color(rgb(0x27272a)).hover(|s| s.bg(rgb(0xe4e4e7)))
                        })
                        .child(label)
                        .on_click(move |_e, _w, cx| {
                            content_w.update(cx, |f, cx| f.accept_word(&word, cx));
                            state_w.update(cx, |st, cx| {
                                st.dict.close();
                                cx.notify();
                            });
                        })
                        .into_any_element(),
                );
            }
            editor_col = editor_col.child(
                div()
                    .border_t_1()
                    .border_color(rgb(0xe4e4e7))
                    .bg(rgb(0xfbfcfe))
                    .px_3()
                    .py_2()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .children(dict_row),
            );
        }

        div()
            .flex_1()
            .min_h(px(0.0))
            .flex()
            .child(sidebar)
            .child(editor_col)
            .into_any_element()
    }

    // ---------- 文件库 ----------

    fn render_oss(&self, st: &AppState) -> AnyElement {
        if std::env::var("GPUI_RENDER_LOG").is_ok() {
            eprintln!(
                "[render-oss] creds={} err='{}' dirs={} files={}",
                st.oss.creds.is_some(),
                st.oss.creds_err,
                st.oss.dirs.len(),
                st.oss.files.len(),
            );
        }
        let state = self.state.clone();

        // 顶部:面包屑 + 操作
        let mut crumbs: Vec<AnyElement> = vec![Self::btn("📁 根目录")
            .on_click({
                let st_e = state.clone();
                move |_e, _w, cx| {
                    st_e.update(cx, |st, cx| {
                        st.oss.close_dialogs();
                        st.oss_list(String::new());
                        cx.notify();
                    });
                }
            })
            .into_any_element()];
        let parts: Vec<String> = st
            .oss
            .prefix
            .trim_end_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();
        let mut acc = String::new();
        for p in &parts {
            acc.push_str(p);
            acc.push('/');
            let target = acc.clone();
            crumbs.push(div().text_color(rgb(0xa1a1aa)).child("/").into_any_element());
            crumbs.push(
                Self::btn(p.clone())
                    .on_click({
                        let st_e = state.clone();
                        move |_e, _w, cx| {
                            st_e.update(cx, |st, cx| {
                                st.oss.close_dialogs();
                                st.oss_list(target.clone());
                                cx.notify();
                            });
                        }
                    })
                    .into_any_element(),
            );
        }

        let dlg = self.dialog_input.clone();
        let state_nf = state.clone();
        let state_nd = state.clone();
        let state_rf = state.clone();
        let dlg2 = self.dialog_input.clone();

        let oss_bar = div()
            .h(px(44.0))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(rgb(0xe4e4e7))
            .children(crumbs)
            .child(
                Self::btn("＋ 文件")
                    .on_click(move |_e, window, cx| {
                        dlg.update(cx, |f, cx| f.set_text(String::new(), cx));
                        let fh = dlg.read(cx).focus_handle();
                        fh.focus(window);
                        state_nf.update(cx, |st, cx| {
                            st.oss.dialog = Some(OssDialog::NewFile(String::new()));
                            cx.notify();
                        });
                    }),
            )
            .child(
                Self::btn("＋ 文件夹").on_click(move |_e, window, cx| {
                    dlg2.update(cx, |f, cx| f.set_text(String::new(), cx));
                    let fh = dlg2.read(cx).focus_handle();
                    fh.focus(window);
                    state_nd.update(cx, |st, cx| {
                        st.oss.dialog = Some(OssDialog::NewFolder(String::new()));
                        cx.notify();
                    });
                }),
            )
            .child(Self::btn("刷新").on_click(move |_e, _w, cx| {
                let prefix = state_rf.read_with(cx, |st, _| st.oss.prefix.clone());
                state_rf.update(cx, |st, cx| {
                    st.oss_list(prefix);
                    cx.notify();
                });
            }));

        let cur_file = st.oss.current.as_ref().map(|f| (f.key.clone(), f.dirty, f.saving, f.font_size));

        let state_close = state.clone();
        let oss_bar_final = oss_bar.child(
            div()
                .flex_1()
                .flex()
                .flex_row_reverse()
                .items_center()
                .gap_3()
                .when(!st.oss.status.is_empty(), |d| {
                    d.child(div().text_xs().text_color(rgb(0x71717a)).child(st.oss.status.clone()))
                })
                .when(st.oss.loading, |d| {
                    d.child(div().text_xs().text_color(rgb(0x71717a)).child("加载中…"))
                })
                .when_some(cur_file.clone(), |d, (key, dirty, saving, _)| {
                    let name = key.rsplit('/').next().unwrap_or(&key).to_string();
                    let d = if dirty {
                        d.child(div().text_xs().text_color(rgb(0xd97706)).child("● 未保存"))
                    } else if saving {
                        d.child(div().text_xs().text_color(rgb(0x71717a)).child("保存中…"))
                    } else {
                        d
                    };
                    d.child(div().text_xs().text_color(rgb(0x27272a)).child(name)).child(
                        Self::btn("✕ 关闭文件").on_click(move |_e, _w, cx| {
                            state_close.update(cx, |st, cx| {
                                st.oss_save_now();
                                st.oss.current = None;
                                cx.notify();
                            });
                        }),
                    )
                }),
        );

        // 左:文件列表(uniform_list 自带滚动)
        enum OssItem {
            Msg(String, bool),                 // 文本, 是否错误色
            Dir { full: String, name: String },
            File { key: String, name: String, editable: bool },
        }
        let mut items: Vec<OssItem> = Vec::new();
        if st.oss.creds.is_none() {
            if st.oss.creds_err.is_empty() {
                items.push(OssItem::Msg("加载对象存储配置…".into(), false));
            } else {
                items.push(OssItem::Msg(
                    format!("对象存储未配置:{}", st.oss.creds_err),
                    true,
                ));
            }
        } else {
            for d in st.oss.dirs.clone() {
                let name = d.trim_start_matches(&st.oss.prefix).to_string();
                items.push(OssItem::Dir { full: d, name });
            }
            for f in st.oss.files.clone() {
                if f.name.ends_with(".settings.json") {
                    continue;
                }
                let editable = f.name.ends_with(".md")
                    || f.name.ends_with(".txt")
                    || f.name.ends_with(".markdown");
                items.push(OssItem::File { key: f.key, name: f.name, editable });
            }
            if items.is_empty() && !st.oss.loading {
                items.push(OssItem::Msg("空目录,点上方「＋ 文件 / ＋ 文件夹」新建".into(), false));
            }
        }
        let st_list = state.clone();
        let list = gpui::uniform_list(
            SharedString::from("oss-list"),
            items.len(),
            move |range, _window, _cx| {
                let mut rows: Vec<AnyElement> = Vec::new();
                for i in range {
                    match &items[i] {
                        OssItem::Msg(text, is_err) => rows.push(
                            div()
                                .px_3()
                                .py_2()
                                .text_xs()
                                .text_color(if *is_err { rgb(0xdc2626) } else { rgb(0x71717a) })
                                .child(text.clone())
                                .into_any_element(),
                        ),
                        OssItem::Dir { full, name } => {
                            let full = full.clone();
                            let name = name.clone();
                            let name2 = name.clone();
                            let st_open = st_list.clone();
                            let st_del = st_list.clone();
                            rows.push(
                                div()
                                    .id(SharedString::from(format!("oss-dir-{name}")))
                                    .px_2()
                                    .py_1()
                                    .mx_1()
                                    .rounded_md()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .hover(|s| s.bg(rgb(0xf4f4f5)))
                                    .child(
                                        div()
                                            .id(SharedString::from(format!("oss-dir-open-{name}")))
                                            .text_sm()
                                            .text_color(rgb(0x1d4ed8))
                                            .cursor(gpui::CursorStyle::PointingHand)
                                            .child(format!("📁 {name}"))
                                            .on_click(move |_e, _w, cx| {
                                                st_open.update(cx, |st, cx| {
                                                    st.oss.close_dialogs();
                                                    st.oss_list(full.clone());
                                                    cx.notify();
                                                });
                                            }),
                                    )
                                    .child(
                                        div()
                                            .id(SharedString::from(format!("oss-dir-del-{name}")))
                                            .text_xs()
                                            .text_color(rgb(0xa1a1aa))
                                            .cursor(gpui::CursorStyle::PointingHand)
                                            .hover(|s| s.text_color(rgb(0xdc2626)))
                                            .child("✕")
                                            .on_click(move |_e, _w, cx| {
                                                st_del.update(cx, |st, cx| {
                                                    st.oss.dialog =
                                                        Some(OssDialog::ConfirmDelete(name2.clone(), true));
                                                    cx.notify();
                                                });
                                            }),
                                    )
                                    .into_any_element(),
                            );
                        }
                        OssItem::File { key, name, editable } => {
                            let key = key.clone();
                            let key_open = key.clone();
                            let key_del = key.clone();
                            let name = name.clone();
                            let editable = *editable;
                            let icon = if name.ends_with(".md") { "📄" } else { "📎" };
                            let st_open = st_list.clone();
                            let st_del = st_list.clone();
                            rows.push(
                                div()
                                    .id(SharedString::from(format!("oss-file-{key}")))
                                    .px_2()
                                    .py_1()
                                    .mx_1()
                                    .rounded_md()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .hover(|s| s.bg(rgb(0xf4f4f5)))
                                    .child(
                                        div()
                                            .id(SharedString::from(format!("oss-file-open-{key}")))
                                            .text_sm()
                                            .text_color(rgb(0x27272a))
                                            .cursor(gpui::CursorStyle::PointingHand)
                                            .child(format!("{icon} {name}"))
                                            .on_click(move |_e, _w, cx| {
                                                if editable {
                                                    let key = key_open.clone();
                                                    st_open.update(cx, |st, cx| {
                                                        st.oss.close_dialogs();
                                                        st.oss_open(key);
                                                        cx.notify();
                                                    });
                                                } else {
                                                    st_open.update(cx, |st, cx| {
                                                        st.oss.status =
                                                            "该类型暂不支持在线编辑(图片/Office 等后续版本)"
                                                                .into();
                                                        cx.notify();
                                                    });
                                                }
                                            }),
                                    )
                                    .child(
                                        div()
                                            .id(SharedString::from(format!("oss-file-del-{key_del}")))
                                            .text_xs()
                                            .text_color(rgb(0xa1a1aa))
                                            .cursor(gpui::CursorStyle::PointingHand)
                                            .hover(|s| s.text_color(rgb(0xdc2626)))
                                            .child("✕")
                                            .on_click(move |_e, _w, cx| {
                                                st_del.update(cx, |st, cx| {
                                                    st.oss.dialog =
                                                        Some(OssDialog::ConfirmDelete(name.clone(), false));
                                                    cx.notify();
                                                });
                                            }),
                                    )
                                    .into_any_element(),
                            );
                        }
                    }
                }
                rows
            },
        );

        let sidebar = div()
            .w(px(280.0))
            .h_full()
            .border_r_1()
            .border_color(rgb(0xe4e4e7))
            .bg(rgb(0xfbfcfe))
            .py_1()
            .child(list);

        // 右:编辑器
        let cur_key = cur_file.map(|(k, ..)| k);
        let editor_open = !st.editor.open_id.is_empty();
        let editor_for_this = editor_open && cur_key.as_deref() == Some(st.editor.open_id.as_str());
        let editor_other = editor_open && !editor_for_this;

        let state_v = state.clone();
        let state_disc = state.clone();
        let vditor_row: AnyElement = if editor_for_this {
            div()
                .h(px(36.0))
                .px_3()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(0x16a34a))
                        .child("✅ Vditor 编辑中(浏览器窗口,自动保存回桶)"),
                )
                .child(Self::btn("✕ 断开编辑器").on_click(move |_e, _w, cx| {
                    state_disc.update(cx, |st, cx| {
                        st.close_editor();
                        cx.notify();
                    });
                }))
                .into_any_element()
        } else {
            div()
                .h(px(36.0))
                .px_3()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    Self::btn("📝 用 Vditor 编辑").on_click(move |_e, _w, cx| {
                        let snap = state_v
                            .read_with(cx, |st, _| {
                                st.oss.current.as_ref().map(|f| {
                                    let key = f.key.clone();
                                    let title =
                                        key.rsplit('/').next().unwrap_or(&key).to_string();
                                    (key, title, f.content.clone())
                                })
                            });
                        if let Some((key, title, content)) = snap {
                            state_v.update(cx, |st, cx| {
                                st.open_vditor(key, title, content);
                                cx.notify();
                            });
                        }
                    }),
                )
                .when(editor_other, |d| {
                    d.child(
                        div()
                            .text_sm()
                            .text_color(rgb(0xd97706))
                            .child("⚠️ 编辑器开着别的文档"),
                    )
                })
                .into_any_element()
        };

        let state_fs1 = state.clone();
        let state_fs2 = state.clone();
        let font_size = st.oss.current.as_ref().map(|f| f.font_size).unwrap_or(16);

        let mut editor_col = div()
            .flex_1()
            .h_full()
            .flex()
            .flex_col()
            .child(vditor_row)
            .child(
                div()
                    .h(px(36.0))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().text_xs().text_color(rgb(0x71717a)).child("字号"))
                    .child(Self::btn("A−").on_click(move |_e, _w, cx| {
                        state_fs1.update(cx, |st, cx| {
                            if let Some(f) = &mut st.oss.current {
                                f.font_size = (f.font_size - 1).max(shared::MIN_FONT_SIZE);
                                f.mark_edit();
                            }
                            cx.notify();
                        });
                    }))
                    .child(Self::btn("A+").on_click(move |_e, _w, cx| {
                        state_fs2.update(cx, |st, cx| {
                            if let Some(f) = &mut st.oss.current {
                                f.font_size = (f.font_size + 1).min(shared::MAX_FONT_SIZE);
                                f.mark_edit();
                            }
                            cx.notify();
                        });
                    }))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x71717a))
                            .child(format!("({font_size} 键入自动保存)")),
                    ),
            )
            .child(div().mx_3().h(px(1.0)).bg(rgb(0xe4e4e7)))
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.0))
                    .p_3()
                    .child(div().h_full().child(self.oss_content_input.clone())),
            );

        if editor_for_this {
            editor_col = editor_col.child(
                div()
                    .px_3()
                    .pb_2()
                    .text_xs()
                    .text_color(rgb(0x71717a))
                    .child("正文正在 Vditor 浏览器窗口中编辑"),
            );
        }

        div()
            .flex_1()
            .min_h(px(0.0))
            .flex()
            .flex_col()
            .child(oss_bar_final)
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.0))
                    .flex()
                    .child(sidebar)
                    .child(editor_col),
            )
            .into_any_element()
    }

    // ---------- 画图 ----------

    fn render_draw(&self, st: &AppState) -> AnyElement {
        let state = self.state.clone();

        if st.draw.ed.is_none() {
            // 画廊
            let state_new = state.clone();
            let state_rf = state.clone();
            let dlg = self.dialog_input.clone();
            let mut buttons: Vec<AnyElement> = vec![
                Self::btn("＋ 新建画板")
                    .on_click(move |_e, window, cx| {
                        dlg.update(cx, |f, cx| {
                            f.set_text(format!("未命名 {}", draw::chrono_now()), cx);
                        })
                        ;
                        let fh = dlg.read(cx).focus_handle();
                        fh.focus(window);
                        state_new.update(cx, |st, cx| {
                            st.draw.new_dialog = true;
                            cx.notify();
                        });
                    })
                    .into_any_element(),
                Self::btn("🔄 刷新")
                    .on_click(move |_e, _w, cx| {
                        state_rf.update(cx, |st, cx| {
                            st.draw_load_list();
                            cx.notify();
                        });
                    })
                    .into_any_element(),
            ];
            if st.draw.loading {
                buttons.push(
                    div().text_xs().text_color(rgb(0x71717a)).child("加载中…").into_any_element(),
                );
            }
            let rows_data: Vec<(String, String, String)> = st
                .draw
                .list
                .iter()
                .map(|m| {
                    (
                        m.id.clone(),
                        if m.name.is_empty() { "(未命名)".to_string() } else { m.name.clone() },
                        m.updated_at.clone(),
                    )
                })
                .collect();
            let empty = rows_data.is_empty() && !st.draw.loading;
            let st_rows = state.clone();
            let name_rows = self.name_input.clone();
            let list = gpui::uniform_list(
                SharedString::from("draw-list"),
                rows_data.len() + if empty { 1 } else { 0 },
                move |range, _window, _cx| {
                    let mut out: Vec<AnyElement> = Vec::new();
                    for i in range {
                        if i >= rows_data.len() {
                            out.push(
                                div()
                                    .px_3()
                                    .py_2()
                                    .text_xs()
                                    .text_color(rgb(0xa1a1aa))
                                    .child("还没有画板,点「＋ 新建画板」开始")
                                    .into_any_element(),
                            );
                            continue;
                        }
                        let (id, title, time) = rows_data[i].clone();
                        let id_open = id.clone();
                        let id_del = id.clone();
                        let st_open = st_rows.clone();
                        let st_del = st_rows.clone();
                        let name_i = name_rows.clone();
                        out.push(
                            div()
                                .id(SharedString::from(format!("draw-row-{id}")))
                                .px_2()
                                .py_1()
                                .mx_1()
                                .rounded_md()
                                .flex()
                                .items_center()
                                .gap_3()
                                .hover(|s| s.bg(rgb(0xf4f4f5)))
                                .child(
                                    div()
                                        .id(SharedString::from(format!("draw-open-{id_open}")))
                                        .text_sm()
                                        .text_color(rgb(0x27272a))
                                        .cursor(gpui::CursorStyle::PointingHand)
                                        .child(format!("🎨 {title}"))
                                        .on_click(move |_e, _w, cx| {
                                            name_i.update(cx, |f, cx| {
                                                f.set_text(title.clone(), cx)
                                            });
                                            st_open.update(cx, |st, cx| {
                                                st.draw_open(id_open.clone());
                                                cx.notify();
                                            });
                                        }),
                                )
                                .child(
                                    div().text_xs().text_color(rgb(0xa1a1aa)).child(time),
                                )
                                .child(
                                    div()
                                        .id(SharedString::from(format!("draw-del-{id_del}")))
                                        .text_xs()
                                        .text_color(rgb(0xa1a1aa))
                                        .cursor(gpui::CursorStyle::PointingHand)
                                        .hover(|s| s.text_color(rgb(0xdc2626)))
                                        .child("✕")
                                        .on_click(move |_e, _w, cx| {
                                            st_del.update(cx, |st, cx| {
                                                st.draw.confirm_delete = Some(id_del.clone());
                                                cx.notify();
                                            });
                                        }),
                                )
                                .into_any_element(),
                        );
                    }
                    out
                },
            );
            return div()
                .flex_1()
                .flex()
                .flex_col()
                .child(
                    div()
                        .h(px(48.0))
                        .px_3()
                        .flex()
                        .items_center()
                        .gap_2()
                        .border_b_1()
                        .border_color(rgb(0xe4e4e7))
                        .children(buttons),
                )
                .child(div().flex_1().py_1().child(list))
                .into_any_element();
        }

        // 编辑器
        let ed = st.draw.ed.as_ref().unwrap();
        let ed_dirty = ed.dirty;
        let ed_saving = ed.saving;

        let state_back = state.clone();
        let state_undo = state.clone();
        let state_clear = state.clone();

        let mut bar = div()
            .h(px(44.0))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(rgb(0xe4e4e7))
            .child(
                Self::btn("🎨 画廊").on_click(move |_e, _w, cx| {
                    state_back.update(cx, |st, cx| {
                        st.draw_save_now();
                        st.draw.ed = None;
                        st.draw_load_list();
                        cx.notify();
                    });
                }),
            )
            .child(div().text_xs().text_color(rgb(0x71717a)).child("名称"))
            .child(div().w(px(180.0)).child(self.name_input.clone()))
            .child(
                Self::btn("↩ 撤销").on_click(move |_e, _w, cx| {
                    state_undo.update(cx, |st, cx| {
                        if let Some(ed) = &mut st.draw.ed {
                            ed.elements.pop();
                            ed.mark_edit();
                        }
                        cx.notify();
                    });
                }),
            )
            .child(
                Self::btn("🗑 清空").on_click(move |_e, _w, cx| {
                    state_clear.update(cx, |st, cx| {
                        if let Some(ed) = &mut st.draw.ed {
                            ed.elements.clear();
                            ed.mark_edit();
                        }
                        cx.notify();
                    });
                }),
            )
            .child(if ed_dirty {
                div().text_xs().text_color(rgb(0xd97706)).child("● 未保存").into_any_element()
            } else if ed_saving {
                div().text_xs().text_color(rgb(0x71717a)).child("保存中…").into_any_element()
            } else {
                div().text_xs().text_color(rgb(0x16a34a)).child("已保存").into_any_element()
            });

        if !st.draw.status.is_empty() {
            bar = bar.child(
                div().text_xs().text_color(rgb(0xdc2626)).child(st.draw.status.clone()),
            );
        }

        // 工具行
        let mut tools: Vec<AnyElement> = Vec::new();
        for (t, icon) in draw::TOOLS {
            let st_t = state.clone();
            let active = st.draw.tool == t;
            tools.push(
                div()
                    .id(SharedString::from(format!("draw-tool-{icon}")))
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .text_base()
                    .cursor(gpui::CursorStyle::PointingHand)
                    .when(active, |d| d.bg(rgb(0x2b3a67)))
                    .when(!active, |d| d.hover(|s| s.bg(rgb(0xe4e4e7))))
                    .child(icon.to_string())
                    .on_click(move |_e, _w, cx| {
                        st_t.update(cx, |st, cx| {
                            st.draw.tool = t;
                            cx.notify();
                        });
                    })
                    .into_any_element(),
            );
        }
        tools.push(div().w(px(4.0)).into_any_element());
        for c in draw::COLORS {
            let st_c = state.clone();
            let selected = st.draw.color == c;
            let color = c.to_string();
            let (r, g, b) = (
                u8::from_str_radix(&c[1..3], 16).unwrap_or(0),
                u8::from_str_radix(&c[3..5], 16).unwrap_or(0),
                u8::from_str_radix(&c[5..7], 16).unwrap_or(0),
            );
            tools.push(
                div()
                    .id(SharedString::from(format!("draw-color-{c}")))
                    .w(px(18.0))
                    .h(px(18.0))
                    .rounded(px(3.0))
                    .border_1()
                    .border_color(if selected { rgb(0x4f46e5) } else { rgb(0xd4d4d8) })
                    .bg(gpui::Rgba {
                        r: r as f32 / 255.0,
                        g: g as f32 / 255.0,
                        b: b as f32 / 255.0,
                        a: 1.0,
                    })
                    .cursor(gpui::CursorStyle::PointingHand)
                    .on_click(move |_e, _w, cx| {
                        st_c.update(cx, |st, cx| {
                            st.draw.color = color.clone();
                            cx.notify();
                        });
                    })
                    .into_any_element(),
            );
        }
        tools.push(div().w(px(4.0)).into_any_element());
        for w in draw::WIDTHS {
            let st_w = state.clone();
            let selected = (st.draw.width - w).abs() < 0.1;
            tools.push(
                div()
                    .id(SharedString::from(format!("draw-width-{w}")))
                    .px_1()
                    .py_0p5()
                    .rounded_md()
                    .cursor(gpui::CursorStyle::PointingHand)
                    .text_color(if selected { rgb(0x4f46e5) } else { rgb(0x71717a) })
                    .hover(|s| s.bg(rgb(0xe4e4e7)))
                    .child(format!("●{}", if w <= 1.0 { "" } else if w <= 3.0 { "●" } else if w <= 5.0 { "●●" } else { "●●●" }))
                    .on_click(move |_e, _w, cx| {
                        st_w.update(cx, |st, cx| {
                            st.draw.width = w;
                            cx.notify();
                        });
                    })
                    .into_any_element(),
            );
        }

        let tool_row = div()
            .h(px(38.0))
            .px_3()
            .flex()
            .items_center()
            .gap_1()
            .border_t_1()
            .border_color(rgb(0xe4e4e7))
            .bg(rgb(0xfafafa))
            .children(tools);

        // 画布(含鼠标交互)
        let geom_slot = st.draw.geom.clone();
        let elements = st.draw.ed.as_ref().map(|e| e.elements.clone()).unwrap_or_default();
        let in_progress = st.draw.ed.as_ref().and_then(|e| e.in_progress.clone());
        let state_cv = state.clone();
        let state_cv2 = state.clone();
        let state_cv3 = state.clone();
        let dlg = self.dialog_input.clone();
        let to_logical = move |pos: Point<Pixels>| -> Option<[f64; 2]> {
            let g = geom_slot.lock().ok()?;
            let (origin, scale) = *g.as_ref()?;
            let x: f32 = (pos.x - origin.x).into();
            let y: f32 = (pos.y - origin.y).into();
            Some([
                (x as f64 / scale as f64).clamp(0.0, draw::CW as f64),
                (y as f64 / scale as f64).clamp(0.0, draw::CH as f64),
            ])
        };
        let to_logical2 = to_logical.clone();

        let canvas_area = div()
            .flex_1()
            .min_h(px(0.0))
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .cursor(gpui::CursorStyle::Crosshair)
            .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                let tool = state_cv.read_with(cx, |st, _| st.draw.tool);
                if tool == DrawTool::Text {
                    if let Some(q) = to_logical(event.position) {
                        dlg.update(cx, |f, cx| f.set_text(String::new(), cx));
                        let fh = dlg.read(cx).focus_handle();
                        fh.focus(window);
                        state_cv.update(cx, |st, cx| {
                            st.draw.text_dialog = Some((q, String::new()));
                            cx.notify();
                        });
                    }
                    return;
                }
                let (color, width) =
                    state_cv.read_with(cx, |st, _| (st.draw.color.clone(), st.draw.width));
                if let Some(q) = to_logical(event.position) {
                    state_cv.update(cx, |st, cx| {
                        if let Some(ed) = &mut st.draw.ed {
                            ed.drawing = true;
                            ed.start = q;
                            ed.in_progress = Some(match tool {
                                DrawTool::Pen => DrawEl::Pen { color: color.clone(), width, points: vec![q] },
                                DrawTool::Eraser => DrawEl::Eraser { color: color.clone(), width, points: vec![q] },
                                DrawTool::Line => DrawEl::Line { color: color.clone(), width, x0: q[0], y0: q[1], x1: q[0], y1: q[1] },
                                DrawTool::Rect => DrawEl::Rect { color: color.clone(), width, x0: q[0], y0: q[1], x1: q[0], y1: q[1] },
                                DrawTool::Ellipse => DrawEl::Ellipse { color: color.clone(), width, x0: q[0], y0: q[1], x1: q[0], y1: q[1] },
                                DrawTool::Text => unreachable!(),
                            });
                        }
                        cx.notify();
                    });
                }
            })
            .on_mouse_move(move |event, _window, cx| {
                let drawing =
                    state_cv2.read_with(cx, |st, _| st.draw.ed.as_ref().map(|e| e.drawing).unwrap_or(false));
                if drawing {
                    if let Some(q) = to_logical2(event.position) {
                        state_cv2.update(cx, |st, cx| {
                            if let Some(ed) = &mut st.draw.ed {
                                if let Some(ip) = &mut ed.in_progress {
                                    match ip {
                                        DrawEl::Pen { points, .. } | DrawEl::Eraser { points, .. } => {
                                            points.push(q);
                                        }
                                        DrawEl::Line { x1, y1, .. }
                                        | DrawEl::Rect { x1, y1, .. }
                                        | DrawEl::Ellipse { x1, y1, .. } => {
                                            *x1 = q[0];
                                            *y1 = q[1];
                                        }
                                        DrawEl::Text { .. } => {}
                                    }
                                }
                            }
                            cx.notify();
                        });
                    }
                }
            })
            .on_mouse_up(MouseButton::Left, move |_event, _window, cx| {
                let was_drawing =
                    state_cv3.read_with(cx, |st, _| st.draw.ed.as_ref().map(|e| e.drawing).unwrap_or(false));
                if was_drawing {
                    state_cv3.update(cx, |st, cx| {
                        if let Some(ed) = &mut st.draw.ed {
                            ed.drawing = false;
                            if let Some(ip) = ed.in_progress.take() {
                                ed.elements.push(ip);
                                ed.mark_edit();
                            }
                        }
                        cx.notify();
                    });
                }
            })
            .child(draw::draw_canvas(elements, in_progress, st.draw.geom.clone()));


        div()
            .flex_1()
            .min_h(px(0.0))
            .flex()
            .flex_col()
            .child(bar)
            .child(canvas_area)
            .child(tool_row)
            .into_any_element()
    }

    // ---------- 对话框 ----------

    fn render_dialogs(&self, st: &AppState) -> Option<AnyElement> {
        let state = self.state.clone();
        let dlg = self.dialog_input.clone();

        if let Some(d) = &st.oss.dialog {
            match d {
                OssDialog::NewFile(_) | OssDialog::NewFolder(_) => {
                    let is_file = matches!(d, OssDialog::NewFile(_));
                    let title = if is_file { "新建文件" } else { "新建文件夹" };
                    let hint = if is_file { "如 笔记.md" } else { "如 资料" };
                    let state_ok = state.clone();
                    let dlg2 = dlg.clone();
                    return Some(
                        Self::overlay(
                            div()
                                .flex()
                                .flex_col()
                                .gap_3()
                                .child(
                                    div()
                                        .text_base()
                                        .font_weight(gpui::FontWeight::BOLD)
                                        .child(title),
                                )
                                .child(div().text_xs().text_color(rgb(0x71717a)).child(hint))
                                .child(dlg)
                                .child(
                                    div()
                                        .flex()
                                        .gap_2()
                                        .child(Self::primary_btn("确定").on_click(move |_e, _w, cx| {
                                            let name =
                                                dlg2.read_with(cx, |f, _| f.text());
                                            state_ok.update(cx, |st, cx| {
                                                st.oss_create(!is_file, name);
                                                cx.notify();
                                            });
                                        }))
                                        .child(Self::btn("取消").on_click(move |_e, _w, cx| {
                                            state.update(cx, |st, cx| {
                                                st.oss.dialog = None;
                                                cx.notify();
                                            });
                                        })),
                                ),
                        )
                        .into_any_element(),
                    );
                }
                OssDialog::ConfirmDelete(name, is_dir) => {
                    let name = name.clone();
                    let is_dir = *is_dir;
                    let state_ok = state.clone();
                    return Some(
                        Self::overlay(
                            div()
                                .flex()
                                .flex_col()
                                .gap_3()
                                .child(div().text_base().child(if is_dir {
                                    format!("删除文件夹 {name}/ 及其全部内容?")
                                } else {
                                    format!("删除 {name}?")
                                }))
                                .child(
                                    div()
                                        .flex()
                                        .gap_2()
                                        .child(Self::primary_btn("删除").on_click(move |_e, _w, cx| {
                                            state_ok.update(cx, |st, cx| {
                                                st.oss_delete(name.clone(), is_dir);
                                                cx.notify();
                                            });
                                        }))
                                        .child(Self::btn("取消").on_click(move |_e, _w, cx| {
                                            state.update(cx, |st, cx| {
                                                st.oss.dialog = None;
                                                cx.notify();
                                            });
                                        })),
                                ),
                        )
                        .into_any_element(),
                    );
                }
            }
        }

        if st.draw.new_dialog {
            let state_ok = state.clone();
            let dlg2 = dlg.clone();
            return Some(
                Self::overlay(
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .text_base()
                                .font_weight(gpui::FontWeight::BOLD)
                                .child("新建画板"),
                        )
                        .child(dlg)
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(Self::primary_btn("创建").on_click(move |_e, _w, cx| {
                                    let name = dlg2.read_with(cx, |f, _| f.text());
                                    if name.trim().is_empty() {
                                        return;
                                    }
                                    let id = state::new_note_id();
                                    state_ok.update(cx, |st, cx| {
                                        st.draw.new_dialog = false;
                                        let mut ed = DrawEd::new(id.clone());
                                        ed.name = name.trim().to_string();
                                        ed.dirty = true;
                                        st.draw.ed = Some(ed);
                                        st.draw_open(id);
                                        cx.notify();
                                    });
                                }))
                                .child(Self::btn("取消").on_click(move |_e, _w, cx| {
                                    state.update(cx, |st, cx| {
                                        st.draw.new_dialog = false;
                                        cx.notify();
                                    });
                                })),
                        ),
                )
                .into_any_element(),
            );
        }

        if let Some(did) = st.draw.confirm_delete.clone() {
            let state_ok = state.clone();
            return Some(
                Self::overlay(
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(div().text_base().child(format!("删除画板 {did}?")))
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(Self::primary_btn("删除").on_click(move |_e, _w, cx| {
                                    state_ok.update(cx, |st, cx| {
                                        st.draw_delete(did.clone());
                                        cx.notify();
                                    });
                                }))
                                .child(Self::btn("取消").on_click(move |_e, _w, cx| {
                                    state.update(cx, |st, cx| {
                                        st.draw.confirm_delete = None;
                                        cx.notify();
                                    });
                                })),
                        ),
                )
                .into_any_element(),
            );
        }

        if st.draw.text_dialog.is_some() {
            let state_ok = state.clone();
            let dlg2 = dlg.clone();
            return Some(
                Self::overlay(
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .text_base()
                                .font_weight(gpui::FontWeight::BOLD)
                                .child("文字"),
                        )
                        .child(dlg)
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(Self::primary_btn("确定").on_click(move |_e, _w, cx| {
                                    let text = dlg2.read_with(cx, |f, _| f.text());
                                    let text: String = text.trim().chars().take(200).collect();
                                    state_ok.update(cx, |st, cx| {
                                        let pos = st.draw.text_dialog.take().map(|(p, _)| p);
                                        if let Some(pos) = pos {
                                            if !text.is_empty() {
                                                if let Some(ed) = &mut st.draw.ed {
                                                    ed.elements.push(DrawEl::Text {
                                                        color: st.draw.color.clone(),
                                                        x: pos[0],
                                                        y: pos[1],
                                                        font_size: 24.0,
                                                        text,
                                                    });
                                                    ed.mark_edit();
                                                }
                                            }
                                        }
                                        cx.notify();
                                    });
                                }))
                                .child(Self::btn("取消").on_click(move |_e, _w, cx| {
                                    state.update(cx, |st, cx| {
                                        st.draw.text_dialog = None;
                                        cx.notify();
                                    });
                                })),
                        ),
                )
                .into_any_element(),
            );
        }

        None
    }
}

impl Render for RootView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<RootView>) -> impl IntoElement {
        let screen = self.state.read(cx).screen;
        match screen {
            Screen::Login => {
                if std::env::var("GPUI_RENDER_LOG").is_ok() {
                    eprintln!("[render] login busy={}", self.state.read(cx).busy);
                }
                let (busy, login_err) =
                    self.state.read_with(cx, |st, _| (st.busy, st.login_err.clone()));
                self.render_login(busy, &login_err)
            }
            Screen::Main => {
                let view = self.state.read(cx).view;
                if std::env::var("GPUI_RENDER_LOG").is_ok() {
                    eprintln!("[render] view={view:?}");
                }
                let toolbar = self.render_toolbar(self.state.read(cx));
                let body = match view {
                    View::Notes => self.render_notes(self.state.read(cx)),
                    View::Oss => self.render_oss(self.state.read(cx)),
                    View::Draw => self.render_draw(self.state.read(cx)),
                };
                let dialog = self.render_dialogs(self.state.read(cx));
                let mut root = div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .bg(rgb(0xffffff))
                    .child(toolbar)
                    .child(body);
                if let Some(d) = dialog {
                    root = root.child(deferred(d).with_priority(1));
                }
                root.into_any_element()
            }
        }
    }
}

fn main() {
    let app = Application::new();
    app.run(|cx: &mut App| {
        let bounds = Bounds::new(point(px(60.0), px(60.0)), Size {
            width: px(1280.0),
            height: px(860.0),
        });
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("urlnote".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| RootView::new(window, cx)),
        )
        .expect("open window");
    });
}
