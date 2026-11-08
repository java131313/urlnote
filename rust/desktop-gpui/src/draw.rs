//! 画图 —— 数据契约与 react DrawPage / egui 版一致(kind 小写标签,
//! fontSize 驼峰),走 server /api/drawing(s)。画布用 GPUI canvas,
//! 元素回放:折线/直线 = 粗四边形条带 + 圆端帽,矩形 = 描边 quad,
//! 椭圆 = 48 边形折线近似,橡皮 = 背景色粗线,文字 = text_system。

use gpui::prelude::*;
use gpui::{
    App, BorderStyle, Corners, Edges, Hsla, Pixels, Point, Window, canvas, font, point, px, rgb,
    size,
};
use serde::{Deserialize, Serialize};

pub const CW: f32 = 960.0;
pub const CH: f32 = 600.0;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DrawMeta {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DrawDoc {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub width: i64,
    #[serde(default)]
    pub height: i64,
    #[serde(default)]
    pub elements: Vec<DrawEl>,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum DrawEl {
    #[serde(rename = "pen")]
    Pen { color: String, width: f64, points: Vec<[f64; 2]> },
    #[serde(rename = "eraser")]
    Eraser { color: String, width: f64, points: Vec<[f64; 2]> },
    #[serde(rename = "line")]
    Line { color: String, width: f64, x0: f64, y0: f64, x1: f64, y1: f64 },
    #[serde(rename = "rect")]
    Rect { color: String, width: f64, x0: f64, y0: f64, x1: f64, y1: f64 },
    #[serde(rename = "ellipse")]
    Ellipse { color: String, width: f64, x0: f64, y0: f64, x1: f64, y1: f64 },
    #[serde(rename = "text")]
    Text {
        color: String,
        x: f64,
        y: f64,
        #[serde(rename = "fontSize")]
        font_size: f64,
        text: String,
    },
}

pub fn hex_hsla(hex: &str) -> Hsla {
    let h = hex.trim_start_matches('#');
    if h.len() == 6 {
        if let Ok(v) = u32::from_str_radix(h, 16) {
            let r = ((v >> 16) & 0xff) as f32 / 255.0;
            let g = ((v >> 8) & 0xff) as f32 / 255.0;
            let b = (v & 0xff) as f32 / 255.0;
            return gpui::Rgba { r, g, b, a: 1.0 }.into();
        }
    }
    gpui::black()
}

pub struct DrawEd {
    pub id: String,
    pub name: String,
    pub elements: Vec<DrawEl>,
    pub dirty: bool,
    pub saving: bool,
    pub last_edit: std::time::Instant,
    pub in_progress: Option<DrawEl>,
    pub drawing: bool,
    pub start: [f64; 2],
}

impl DrawEd {
    pub fn new(id: String) -> Self {
        Self {
            id,
            name: String::new(),
            elements: Vec::new(),
            dirty: false,
            saving: false,
            last_edit: std::time::Instant::now(),
            in_progress: None,
            drawing: false,
            start: [0.0, 0.0],
        }
    }
    pub fn mark_edit(&mut self) {
        self.dirty = true;
        self.last_edit = std::time::Instant::now();
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum DrawTool {
    Pen,
    Line,
    Rect,
    Ellipse,
    Eraser,
    Text,
}

pub struct DrawState {
    pub list: Vec<DrawMeta>,
    pub loading: bool,
    pub ed: Option<DrawEd>,
    pub tool: DrawTool,
    pub color: String,
    pub width: f64,
    pub new_dialog: bool,
    pub text_dialog: Option<([f64; 2], String)>,
    pub confirm_delete: Option<String>,
    pub status: String,
    /// 画布几何(事件处理用,prepaint 时回写):(origin, scale)。
    pub geom: std::sync::Arc<std::sync::Mutex<Option<(Point<Pixels>, f32)>>>,
}

impl DrawState {
    pub fn new() -> Self {
        Self {
            list: Vec::new(),
            loading: false,
            ed: None,
            tool: DrawTool::Pen,
            color: "#000000".into(),
            width: 3.0,
            new_dialog: false,
            text_dialog: None,
            confirm_delete: None,
            status: String::new(),
            geom: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }
    }
}

pub const TOOLS: [(DrawTool, &str); 6] = [
    (DrawTool::Pen, "✏️"),
    (DrawTool::Line, "📏"),
    (DrawTool::Rect, "▭"),
    (DrawTool::Ellipse, "◯"),
    (DrawTool::Eraser, "🧽"),
    (DrawTool::Text, "🔤"),
];
pub const COLORS: [&str; 8] = [
    "#000000", "#ffffff", "#e74c3c", "#f59e0b", "#2ecc71", "#3498db", "#8e44ad", "#7f8c8d",
];
pub const WIDTHS: [f64; 4] = [1.0, 3.0, 5.0, 8.0];

/// 画布:prepaint 记录几何(相对 origin + scale),paint 回放元素(裁剪到画布)。
/// 事件处理用相对坐标,逻辑坐标 = (pos - origin_rel) / scale。
pub fn draw_canvas(
    elements: Vec<DrawEl>,
    in_progress: Option<DrawEl>,
    geom_slot: std::sync::Arc<std::sync::Mutex<Option<(Point<Pixels>, f32)>>>,
) -> impl gpui::IntoElement {
    canvas(
        move |bounds: gpui::Bounds<Pixels>, _window, _cx| {
            let scale = (bounds.size.width / px(CW))
                .min(bounds.size.height / px(CH))
                .min(4.0);
            let cw = px(CW) * scale;
            let ch = px(CH) * scale;
            let origin_rel = point(
                (bounds.size.width - cw) / 2.0,
                (bounds.size.height - ch) / 2.0,
            );
            if let Ok(mut g) = geom_slot.lock() {
                *g = Some((origin_rel, scale));
            }
            (bounds, origin_rel, scale)
        },
        move |_bounds, (bounds, origin_rel, scale), window, cx| {
            let origin = point(
                bounds.origin.x + origin_rel.x,
                bounds.origin.y + origin_rel.y,
            );
            let canvas_bounds = gpui::Bounds::new(origin, size(px(CW) * scale, px(CH) * scale));
            window.paint_quad(gpui::PaintQuad {
                bounds: canvas_bounds,
                corner_radii: Corners::default(),
                background: rgb(0xffffff).into(),
                border_widths: Edges {
                    top: px(1.0),
                    right: px(1.0),
                    bottom: px(1.0),
                    left: px(1.0),
                },
                border_color: rgb(0xc8c8d0).into(),
                border_style: BorderStyle::default(),
            });
            window.with_content_mask(
                Some(gpui::ContentMask {
                    bounds: canvas_bounds,
                }),
                |window| {
                    let all: Vec<&DrawEl> =
                        elements.iter().chain(in_progress.as_ref().into_iter()).collect();
                    for el in all {
                        paint_el(window, cx, el, origin, scale);
                    }
                },
            );
        },
    )
    .flex_1()
    .w_full()
    .h_full()
}

fn map_pt(origin: Point<Pixels>, scale: f32, q: [f64; 2]) -> Point<Pixels> {
    point(origin.x + px(q[0] as f32) * scale, origin.y + px(q[1] as f32) * scale)
}

fn sw(w: f64, scale: f32) -> Pixels {
    (px(w as f32) * scale).max(px(0.5))
}

fn circle_quad(c: Point<Pixels>, r: Pixels) -> gpui::PaintQuad {
    gpui::PaintQuad {
        bounds: gpui::Bounds::new(point(c.x - r, c.y - r), size(r * 2.0, r * 2.0)),
        corner_radii: Corners {
            top_left: r,
            top_right: r,
            bottom_left: r,
            bottom_right: r,
        },
        background: gpui::hsla(0.0, 0.0, 0.0, 0.0).into(),
        border_widths: Edges::default(),
        border_color: gpui::hsla(0.0, 0.0, 0.0, 0.0),
        border_style: BorderStyle::default(),
    }
}

/// 粗线段:垂直方向展开成矩形填充 + 两端圆帽。
pub fn stroke_segment(window: &mut Window, a: Point<Pixels>, b: Point<Pixels>, color: Hsla, width: Pixels) {
    let dx: f32 = (b.x - a.x).into();
    let dy: f32 = (b.y - a.y).into();
    let wh: f32 = width.into();
    let len = (dx * dx + dy * dy).sqrt().max(0.01);
    let nx = -dy / len * (wh / 2.0);
    let ny = dx / len * (wh / 2.0);
    let p0 = point(a.x + px(nx), a.y + px(ny));
    let p1 = point(a.x - px(nx), a.y - px(ny));
    let p2 = point(b.x - px(nx), b.y - px(ny));
    let p3 = point(b.x + px(nx), b.y + px(ny));
    let mut path = gpui::Path::new(p0);
    path.line_to(p1);
    path.line_to(p2);
    path.line_to(p3);
    window.paint_path(path, color);
    let mut c = circle_quad(a, width / 2.0);
    c.background = color.into();
    window.paint_quad(c);
    let mut c2 = circle_quad(b, width / 2.0);
    c2.background = color.into();
    window.paint_quad(c2);
}

/// 折线。
fn stroke_polyline(window: &mut Window, pts: &[Point<Pixels>], color: Hsla, width: Pixels) {
    if pts.is_empty() {
        return;
    }
    if pts.len() == 1 {
        let mut c = circle_quad(pts[0], width / 2.0);
        c.background = color.into();
        window.paint_quad(c);
        return;
    }
    for w in pts.windows(2) {
        stroke_segment(window, w[0], w[1], color, width);
    }
}

/// 矩形描边 quad。
fn stroke_quad(bounds: gpui::Bounds<Pixels>, color: Hsla, width: Pixels) -> gpui::PaintQuad {
    gpui::PaintQuad {
        bounds,
        corner_radii: Corners::default(),
        background: gpui::hsla(0.0, 0.0, 0.0, 0.0).into(),
        border_widths: Edges {
            top: width,
            right: width,
            bottom: width,
            left: width,
        },
        border_color: color,
        border_style: BorderStyle::default(),
    }
}

/// 回放单个元素。
pub fn paint_el(window: &mut Window, cx: &mut App, el: &DrawEl, origin: Point<Pixels>, scale: f32) {
    match el {
        DrawEl::Pen { color, width, points } => {
            let pts: Vec<Point<Pixels>> = points.iter().map(|q| map_pt(origin, scale, *q)).collect();
            stroke_polyline(window, &pts, hex_hsla(color), sw(*width, scale));
        }
        DrawEl::Eraser { width, points, .. } => {
            let pts: Vec<Point<Pixels>> = points.iter().map(|q| map_pt(origin, scale, *q)).collect();
            stroke_polyline(window, &pts, rgb(0xffffff).into(), sw(*width, scale) * 3.0);
        }
        DrawEl::Line { color, width, x0, y0, x1, y1 } => {
            stroke_segment(
                window,
                map_pt(origin, scale, [*x0, *y0]),
                map_pt(origin, scale, [*x1, *y1]),
                hex_hsla(color),
                sw(*width, scale),
            );
        }
        DrawEl::Rect { color, width, x0, y0, x1, y1 } => {
            let a = map_pt(origin, scale, [*x0, *y0]);
            let b = map_pt(origin, scale, [*x1, *y1]);
            let r = gpui::Bounds::new(
                point(a.x.min(b.x), a.y.min(b.y)),
                size((b.x - a.x).abs(), (b.y - a.y).abs()),
            );
            window.paint_quad(stroke_quad(r, hex_hsla(color), sw(*width, scale)));
        }
        DrawEl::Ellipse { color, width, x0, y0, x1, y1 } => {
            let a = map_pt(origin, scale, [*x0, *y0]);
            let b = map_pt(origin, scale, [*x1, *y1]);
            let cxp = (a.x + b.x) / 2.0;
            let cyp = (a.y + b.y) / 2.0;
            let rx = (b.x - a.x).abs() / 2.0;
            let ry = (b.y - a.y).abs() / 2.0;
            let n = 48;
            let pts: Vec<Point<Pixels>> = (0..=n)
                .map(|i| {
                    let t = i as f32 / n as f32 * std::f32::consts::TAU;
                    point(cxp + rx * t.cos(), cyp + ry * t.sin())
                })
                .collect();
            stroke_polyline(window, &pts, hex_hsla(color), sw(*width, scale));
        }
        DrawEl::Text { color, x, y, font_size, text } => {
            let run = gpui::TextRun {
                len: text.len(),
                font: font(".SystemUIFont"),
                color: hex_hsla(color),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            if let Ok(mut lines) = window.text_system().shape_text(
                text.clone().into(),
                px((*font_size as f32) * scale),
                &[run],
                None,
                None,
            ) {
                if let Some(l) = lines.pop() {
                    let lh = px(*font_size as f32) * scale * 1.5;
                    let _ =
                        l.paint(map_pt(origin, scale, [*x, *y]), lh, gpui::TextAlign::Left, None, window, cx);
                }
            }
        }
    }
}

/// 本地时间串(画廊新建命名用)。
pub fn chrono_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    let rem = now.rem_euclid(86400);
    format!("{:02}:{:02}", rem / 3600, (rem % 3600) / 60)
}
