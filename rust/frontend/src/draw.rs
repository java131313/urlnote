//! 画图 —— /draw 画廊与 /draw/{id} 画布,与 react 版(DrawPage.tsx)功能对齐:
//! 画笔/直线/矩形/椭圆/橡皮/文字,颜色与线宽,撤销、清空、800ms 自动保存。
//! 数据走 rust server 的 /api/drawing(s),MongoDB drawings 集合。

use leptos::ev::MouseEvent;
use leptos::html::Canvas;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos::wasm_bindgen::JsCast;
use gloo_net::http::Request;
use serde::{Deserialize, Serialize};
use wasm_bindgen::closure::Closure;
use web_sys::{CanvasRenderingContext2d, RequestCredentials};

use crate::{api_url, new_id, window, BASE_PATH};

const CW: f64 = 960.0;
const CH: f64 = 600.0;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DrawingMeta {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    updated_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DrawingDoc {
    id: String,
    #[serde(default)]
    name: String,
    width: i64,
    height: i64,
    #[serde(default)]
    elements: Vec<serde_json::Value>,
    #[serde(default)]
    updated_at: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind")]
enum El {
    #[serde(rename = "pen")]
    Pen { color: String, width: f64, points: Vec<Vec<f64>> },
    #[serde(rename = "eraser")]
    Eraser { color: String, width: f64, points: Vec<Vec<f64>> },
    #[serde(rename = "line")]
    Line { color: String, width: f64, x0: f64, y0: f64, x1: f64, y1: f64 },
    #[serde(rename = "rect")]
    Rect { color: String, width: f64, x0: f64, y0: f64, x1: f64, y1: f64 },
    #[serde(rename = "ellipse")]
    Ellipse { color: String, width: f64, x0: f64, y0: f64, x1: f64, y1: f64 },
    #[serde(rename = "text")]
    Text { color: String, x: f64, y: f64, #[serde(rename = "fontSize")] font_size: f64, text: String },
}

fn second_segment() -> String {
    let loc = window().location();
    let raw = loc.pathname().unwrap_or_default();
    let path = match raw.strip_prefix(BASE_PATH) {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => rest,
        _ => &raw,
    };
    path.trim_start_matches('/').split('/').nth(1).unwrap_or("").to_string()
}

fn navigate(path: &str) {
    let _ = window().location().set_href(&format!("{BASE_PATH}{path}"));
}

async fn get_json<T: for<'de> Deserialize<'de>>(path: &str) -> Option<T> {
    let resp = Request::get(&api_url(path))
        .credentials(RequestCredentials::Include)
        .send()
        .await
        .ok()?;
    if !resp.ok() {
        return None;
    }
    resp.json::<T>().await.ok()
}

/// 路由入口(main.rs 在首段为 draw 时渲染)。
#[component]
pub fn DrawGate() -> impl IntoView {
    let id = second_segment();
    if id.is_empty() {
        view! { <Gallery/> }.into_any()
    } else {
        view! { <Editor id=id/> }.into_any()
    }
}

// ---------------- 画廊 ----------------

#[component]
fn Gallery() -> impl IntoView {
    let items = RwSignal::new(Vec::<DrawingMeta>::new());
    let status = RwSignal::new(String::from("加载中…"));

    let refresh = move |items: RwSignal<Vec<DrawingMeta>>, status: RwSignal<String>| {
        spawn_local(async move {
            match get_json::<serde_json::Value>("/api/drawings").await {
                Some(v) => {
                    items.set(serde_json::from_value::<Vec<DrawingMeta>>(v["drawings"].clone()).unwrap_or_default());
                    status.set(String::new());
                }
                None => status.set("加载失败(未登录或网络异常)".into()),
            }
        });
    };
    Effect::new(move |_| refresh(items, status));

    let create = move |_| {
        let name = window().prompt_with_message("新画板名称:").ok().flatten().unwrap_or_default();
        let _ = name;
        navigate(&format!("/draw/{}", new_id()));
    };

    view! {
        <header>
            <a href=format!("{BASE_PATH}/") title="返回便签">"📝 便签"</a>
            <a href=format!("{BASE_PATH}/oss") title="对象存储文件库">"🗂 文件库"</a>
            <strong>"🎨 画图"</strong>
            <button type="button" on:click=create title="新建画板">"＋ 新建画板"</button>
            <button type="button" on:click=move |_| refresh(items, status) title="刷新">"🔄"</button>
        </header>
        <div class="draw-gallery">
            <Show when=move || !status.get().is_empty()>
                <p class="draw-status">{move || status.get()}</p>
            </Show>
            <Show when=move || items.get().is_empty() && status.get().is_empty()>
                <p class="draw-status">"还没有画板,点「＋ 新建画板」开始"</p>
            </Show>
            <For each=move || items.get() key=|m| m.id.clone() children=move |m| {
                let href_id = m.id.clone();
                let del_id = m.id.clone();
                let del = move |_| {
                    let items = items;
                    let status = status;
                    let id = del_id.clone();
                    spawn_local(async move {
                        let _ = Request::post(&api_url(&format!("/api/drawing/{id}/delete")))
                            .credentials(RequestCredentials::Include)
                            .send()
                            .await;
                        refresh(items, status);
                    });
                };
                view! {
                    <div class="draw-card">
                        <a class="draw-card-name" href=format!("{BASE_PATH}/draw/{href_id}")>
                            {if m.name.is_empty() { "(未命名)".to_string() } else { m.name.clone() }}
                        </a>
                        <span class="draw-card-time">{m.updated_at}</span>
                        <button type="button" class="tool" on:click=del title="删除">"🗑"</button>
                    </div>
                }
            }/>
        </div>
    }
}

// ---------------- 画布编辑器 ----------------

#[derive(Clone, Default)]
struct EditorState {
    elements: Vec<serde_json::Value>,
    in_progress: Option<serde_json::Value>,
    drawing: bool,
    start: (f64, f64),
    timeout: Option<i32>,
}

fn ctx2d(canvas: &web_sys::HtmlCanvasElement) -> Option<CanvasRenderingContext2d> {
    canvas.get_context("2d").ok()?.map(|c| c.dyn_into::<CanvasRenderingContext2d>().ok()).flatten()
}

fn draw_el(ctx: &CanvasRenderingContext2d, raw: &serde_json::Value) {
    let Ok(el) = serde_json::from_value::<El>(raw.clone()) else { return };
    ctx.set_line_cap("round");
    ctx.set_line_join("round");
    match el {
        El::Pen { color, width, points } | El::Eraser { color, width, points } => {
            let erase = matches!(serde_json::from_value::<El>(raw.clone()), Ok(El::Eraser { .. }));
            if points.is_empty() {
                return;
            }
            if points.len() == 1 {
                ctx.begin_path();
                let _ = ctx.arc(points[0][0], points[0][1], width / 2.0, 0.0, std::f64::consts::PI * 2.0);
                ctx.set_fill_style_str(&color);
                ctx.fill();
                return;
            }
            if erase {
                ctx.set_global_composite_operation("destination-out");
            }
            ctx.set_stroke_style_str(&color);
            ctx.set_line_width(width * if erase { 3.0 } else { 1.0 });
            ctx.begin_path();
            ctx.move_to(points[0][0], points[0][1]);
            for p in &points[1..] {
                ctx.line_to(p[0], p[1]);
            }
            ctx.stroke();
            if erase {
                ctx.set_global_composite_operation("source-over");
            }
        }
        El::Text { color, x, y, font_size, text } => {
            ctx.set_fill_style_str(&color);
            let _ = ctx.set_font(&format!("{font_size}px sans-serif"));
            let _ = ctx.fill_text(&text, x, y);
        }
        El::Line { color, width, x0, y0, x1, y1 } => {
            ctx.set_stroke_style_str(&color);
            ctx.set_line_width(width);
            ctx.begin_path();
            ctx.move_to(x0, y0);
            ctx.line_to(x1, y1);
            ctx.stroke();
        }
        El::Rect { color, width, x0, y0, x1, y1 } => {
            ctx.set_stroke_style_str(&color);
            ctx.set_line_width(width);
            ctx.begin_path();
            let _ = ctx.rect(
                x0.min(x1),
                y0.min(y1),
                (x1 - x0).abs(),
                (y1 - y0).abs(),
            );
            ctx.stroke();
        }
        El::Ellipse { color, width, x0, y0, x1, y1 } => {
            ctx.set_stroke_style_str(&color);
            ctx.set_line_width(width);
            ctx.begin_path();
            let _ = ctx.ellipse(
                (x0 + x1) / 2.0,
                (y0 + y1) / 2.0,
                (x1 - x0).abs() / 2.0,
                (y1 - y0).abs() / 2.0,
                0.0,
                0.0,
                std::f64::consts::PI * 2.0,
            );
            ctx.stroke();
        }
    }
}

fn render_all(canvas: &web_sys::HtmlCanvasElement, st: &EditorState) {
    let Some(ctx) = ctx2d(canvas) else { return };
    ctx.set_fill_style_str("#ffffff");
    ctx.fill_rect(0.0, 0.0, CW, CH);
    for el in &st.elements {
        draw_el(&ctx, el);
    }
    if let Some(ip) = &st.in_progress {
        draw_el(&ctx, ip);
    }
}

#[component]
fn Editor(id: String) -> impl IntoView {
    let canvas_ref = NodeRef::<Canvas>::new();
    let name = RwSignal::new(String::new());
    let status = RwSignal::new(String::from("加载中…"));
    let tool = RwSignal::new("pen".to_string());
    let color = RwSignal::new("#000000".to_string());
    let width = RwSignal::new(3.0f64);
    let state = RwSignal::new(EditorState::default());

    let render = move || {
        if let Some(cv) = canvas_ref.get() {
            render_all(&cv, &state.get());
        }
    };

    // 初始载入
    {
        let id = id.clone();
        Effect::new(move |_| {
            let id = id.clone();
            spawn_local(async move {
                match get_json::<DrawingDoc>(&format!("/api/drawing/{id}")).await {
                    Some(d) => {
                        name.set(d.name);
                        state.write().elements = d.elements;
                        status.set(String::new());
                    }
                    None => status.set("载入失败".into()),
                }
                render();
            });
        });
    }

    let schedule_save = {
        let id = id.clone();
        move |name: RwSignal<String>, status: RwSignal<String>| {
            let id = id.clone();
            let (timeout, elements) = {
                let mut st = state.write();
                if let Some(t) = st.timeout.take() {
                    window().clear_timeout_with_handle(t);
                }
                (st.timeout, st.elements.clone())
            };
            let _ = timeout;
            let name_v = name.get();
            let cb = Closure::once(move || {
                spawn_local(async move {
                    let body = serde_json::json!({ "name": name_v, "width": CW, "height": CH, "elements": elements });
                    let req = Request::put(&api_url(&format!("/api/drawing/{id}")))
                        .credentials(RequestCredentials::Include)
                        .header("Content-Type", "application/json")
                        .body(body.to_string());
                    let ok = match req {
                        Ok(req) => req.send().await.map(|r| r.ok()).unwrap_or(false),
                        Err(_) => false,
                    };
                    status.set(if ok { "已保存".into() } else { "保存失败".into() });
                });
            });
            let t = window()
                .set_timeout_with_callback_and_timeout_and_arguments_0(cb.as_ref().unchecked_ref(), 800)
                .ok();
            cb.forget();
            if let Some(t) = t {
                state.write().timeout = Some(t);
            }
        }
    };

    let mark_dirty = {
        let schedule_save = schedule_save.clone();
        move || {
            status.set("● 未保存".into());
            schedule_save(name, status);
        }
    };

    let pos = |e: &MouseEvent, cv: &web_sys::HtmlCanvasElement| {
        let r = cv.get_bounding_client_rect();
        (
            ((e.client_x() as f64 - r.left()) / r.width()) * CW,
            ((e.client_y() as f64 - r.top()) / r.height()) * CH,
        )
    };

    let on_down = {
        let mark_dirty = mark_dirty.clone();
        move |e: MouseEvent| {
            let Some(cv) = canvas_ref.get() else { return };
            let (x, y) = pos(&e, &cv);
            let t = tool.get();
            let c = color.get();
            let w = width.get();
            if t == "text" {
                let text = window().prompt_with_message("文字内容:").ok().flatten().unwrap_or_default();
                let text: String = text.trim().chars().take(200).collect();
                if !text.is_empty() {
                    state.write().elements.push(serde_json::json!({
                        "kind": "text", "color": c, "x": x, "y": y, "fontSize": 24.0, "text": text,
                    }));
                    mark_dirty();
                }
            } else {
                let mut st = state.write();
                st.drawing = true;
                st.start = (x, y);
                st.in_progress = Some(if t == "pen" || t == "eraser" {
                    serde_json::json!({ "kind": t, "color": c, "width": w, "points": [[x, y]] })
                } else {
                    serde_json::json!({ "kind": t, "color": c, "width": w, "x0": x, "y0": y, "x1": x, "y1": y })
                });
            }
            render();
        }
    };

    let on_move = move |e: MouseEvent| {
        let Some(cv) = canvas_ref.get() else { return };
        let (x, y) = pos(&e, &cv);
        let dirty = {
            let mut st = state.write();
            if !st.drawing {
                return;
            }
            if let Some(ip) = st.in_progress.as_mut() {
                let kind = ip["kind"].as_str().unwrap_or("");
                if kind == "pen" || kind == "eraser" {
                    ip["points"].as_array_mut().map(|a| a.push(serde_json::json!([x, y])));
                } else if kind != "text" {
                    ip["x1"] = serde_json::json!(x);
                    ip["y1"] = serde_json::json!(y);
                }
            }
            true
        };
        if dirty {
            render();
        }
    };

    let on_up = {
        let mark_dirty = mark_dirty.clone();
        move |_| {
            let ip = {
                let mut st = state.write();
                if !st.drawing {
                    return;
                }
                st.drawing = false;
                st.in_progress.take()
            };
            if let Some(ip) = ip {
                state.write().elements.push(ip);
            }
            mark_dirty();
            render();
        }
    };

    let undo = {
        let mark_dirty = mark_dirty.clone();
        move |_| {
            state.write().elements.pop();
            mark_dirty();
            render();
        }
    };

    let clear = {
        let mark_dirty = mark_dirty.clone();
        move |_| {
            if window().confirm_with_message("清空画布?").unwrap_or(false) {
                state.write().elements.clear();
                mark_dirty();
                render();
            }
        }
    };

    let tools: [(&str, &str, &str); 6] = [
        ("pen", "✏️", "画笔"),
        ("line", "📏", "直线"),
        ("rect", "▭", "矩形"),
        ("ellipse", "◯", "椭圆"),
        ("eraser", "🧽", "橡皮"),
        ("text", "🔤", "文字"),
    ];
    let colors = ["#000000", "#ffffff", "#e74c3c", "#f59e0b", "#2ecc71", "#3498db", "#8e44ad", "#7f8c8d"];
    let widths = [1.0f64, 3.0, 5.0, 8.0];

    view! {
        <header>
            <a href=format!("{BASE_PATH}/") title="返回便签">"📝 便签"</a>
            <a href=format!("{BASE_PATH}/oss") title="对象存储文件库">"🗂 文件库"</a>
            <a href=format!("{BASE_PATH}/draw") title="返回画廊">"🎨 画廊"</a>
            <strong>"🎨 画图"</strong>
            <input
                class="title-input"
                placeholder="画板名称"
                prop:value=move || name.get()
                on:input=move |e| name.set(event_target_value(&e))
                on:blur=move |_| mark_dirty()
            />
            <button type="button" on:click=undo title="撤销最后一笔">"↩ 撤销"</button>
            <button type="button" on:click=clear title="清空画布">"🗑 清空"</button>
            <span class="spacer"></span>
            <span class="draw-status">{move || status.get()}</span>
        </header>
        <div class="draw-tools">
            {tools.into_iter().map(|(id_, label, tip)| {
                let set = move |_| { tool.set(id_.to_string()); };
                view! {
                    <button type="button" class:tool-active=move || tool.get() == id_ title=tip on:click=set>
                        {label}
                    </button>
                }
            }).collect::<Vec<_>>()}
            <span class="draw-sep"></span>
            {colors.into_iter().map(|c| {
                let pick = move |_| color.set(c.to_string());
                view! {
                    <button type="button" class="draw-color" class:color-active=move || color.get() == c
                        style:background=c title=c on:click=pick></button>
                }
            }).collect::<Vec<_>>()}
            <span class="draw-sep"></span>
            {widths.into_iter().map(|w| {
                let pick = move |_| width.set(w);
                view! {
                    <button type="button" class:tool-active=move || (width.get() - w).abs() < 0.1
                        title=format!("线宽 {w}") on:click=pick>
                        <span style:font-size=format!("{}px", 8.0 + w * 2.0)>"●"</span>
                    </button>
                }
            }).collect::<Vec<_>>()}
        </div>
        <div class="draw-stage">
            <canvas
                node_ref=canvas_ref
                class="draw-canvas"
                width=CW.to_string()
                height=CH.to_string()
                on:mousedown=on_down
                on:mousemove=on_move
                on:mouseup=on_up.clone()
                on:mouseleave=on_up
            ></canvas>
        </div>
    }
}
