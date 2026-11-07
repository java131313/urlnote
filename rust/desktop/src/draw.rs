//! 桌面版画图 —— 与 react DrawPage 功能对齐:画廊 + 画布。
//! 画笔/直线/矩形/椭圆/橡皮/文字,颜色/线宽,撤销/清空,800ms 自动保存。
//! 数据契约与 react 完全一致(kind 小写标签,fontSize 驼峰),走 server
//! /api/drawing(s)。画布用 egui Painter 矢量回放;橡皮在白色背景上等价
//! 于 destination-out(背景色粗线)。

use eframe::egui::{self, Align2, Color32, CornerRadius, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use serde::{Deserialize, Serialize};

use crate::{go, Msg, UrlnoteApp};

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
    Text { color: String, x: f64, y: f64, #[serde(rename = "fontSize")] font_size: f64, text: String },
}

fn color_of(hex: &str) -> Color32 {
    Color32::from_hex(hex).unwrap_or(Color32::BLACK)
}

/// 用 Painter 回放单个元素(橡皮 = 背景色粗线,白色画布下与擦除等价)。
/// 元素坐标是画布逻辑坐标(0..960),需映射到屏幕:origin + q*scale。
pub fn paint_el(p: &egui::Painter, el: &DrawEl, origin: Pos2, scale: f32) {
    let map = |q: [f64; 2]| Pos2::new(origin.x + q[0] as f32 * scale, origin.y + q[1] as f32 * scale);
    let sw = |w: f64| (w as f32 * scale).max(0.5);
    match el {
        DrawEl::Pen { color, width, points } => {
            let pts: Vec<Pos2> = points.iter().map(|q| map(*q)).collect();
            if pts.len() == 1 {
                p.circle_filled(pts[0], sw(*width) / 2.0, color_of(color));
            } else if pts.len() > 1 {
                p.line(pts, Stroke::new(sw(*width), color_of(color)));
            }
        }
        DrawEl::Eraser { width, points, .. } => {
            let pts: Vec<Pos2> = points.iter().map(|q| map(*q)).collect();
            if pts.len() > 1 {
                p.line(pts, Stroke::new(sw(*width) * 3.0, Color32::WHITE));
            }
        }
        DrawEl::Line { color, width, x0, y0, x1, y1 } => {
            p.line_segment([map([*x0, *y0]), map([*x1, *y1])], Stroke::new(sw(*width), color_of(color)));
        }
        DrawEl::Rect { color, width, x0, y0, x1, y1 } => {
            p.rect_stroke(
                Rect::from_two_pos(map([*x0, *y0]), map([*x1, *y1])),
                CornerRadius::ZERO,
                Stroke::new(sw(*width), color_of(color)),
                StrokeKind::Inside,
            );
        }
        DrawEl::Ellipse { color, width, x0, y0, x1, y1 } => {
            let r = Rect::from_two_pos(map([*x0, *y0]), map([*x1, *y1]));
            p.add(egui::Shape::Ellipse(egui::epaint::EllipseShape {
                center: r.center(),
                radius: Vec2::new(r.width() / 2.0, r.height() / 2.0),
                fill: Color32::TRANSPARENT,
                stroke: Stroke::new(sw(*width), color_of(color)),
            }));
        }
        DrawEl::Text { color, x, y, font_size, text } => {
            p.text(
                map([*x, *y]),
                Align2::LEFT_TOP,
                text,
                egui::FontId::proportional(*font_size as f32 * scale),
                color_of(color),
            );
        }
    }
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
    /// 新建画板命名弹窗(输入 + 待创建)
    pub new_dialog: bool,
    pub new_name: String,
    /// 文字工具弹窗(画布坐标 + 输入)
    pub text_dialog: Option<([f64; 2], String)>,
    /// 删除确认(画板 id)
    pub confirm_delete: Option<String>,
    pub status: String,
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
            new_name: String::new(),
            text_dialog: None,
            confirm_delete: None,
            status: String::new(),
        }
    }
}

const TOOLS: [(DrawTool, &str, &str); 6] = [
    (DrawTool::Pen, "✏️", "画笔"),
    (DrawTool::Line, "📏", "直线"),
    (DrawTool::Rect, "▭", "矩形"),
    (DrawTool::Ellipse, "◯", "椭圆"),
    (DrawTool::Eraser, "🧽", "橡皮"),
    (DrawTool::Text, "🔤", "文字(点击画布)"),
];
const COLORS: [&str; 8] = ["#000000", "#ffffff", "#e74c3c", "#f59e0b", "#2ecc71", "#3498db", "#8e44ad", "#7f8c8d"];
const WIDTHS: [f64; 4] = [1.0, 3.0, 5.0, 8.0];

impl UrlnoteApp {
    pub fn enter_draw(&mut self) {
        self.view = crate::View::Draw;
        self.draw_load_list();
    }

    pub fn draw_load_list(&mut self) {
        self.draw.loading = true;
        let tx = self.rx_tx();
        let base = self.api_base.clone();
        go(&self.client, &self.ctx, &tx, move |c| {
            let r = c.get(format!("{base}/api/drawings")).send();
            Msg::DrawList(match r {
                Ok(resp) if resp.status().is_success() => {
                    resp.json::<serde_json::Value>()
                        .map(|v| serde_json::from_value::<Vec<DrawMeta>>(v["drawings"].clone()).unwrap_or_default())
                        .map_err(|e| e.to_string())
                }
                Ok(resp) => Err(format!("HTTP {}", resp.status())),
                Err(e) => Err(format!("网络错误: {e}")),
            })
        });
    }

    pub fn draw_open(&mut self, id: String) {
        self.draw.ed = Some(DrawEd::new(id.clone()));
        let tx = self.rx_tx();
        let base = self.api_base.clone();
        go(&self.client, &self.ctx, &tx, move |c| {
            let r = c.get(format!("{base}/api/drawing/{id}")).send();
            Msg::DrawGot(match r {
                Ok(resp) if resp.status().is_success() => resp.json::<DrawDoc>().map_err(|e| e.to_string()),
                Ok(resp) => Err(format!("HTTP {}", resp.status())),
                Err(e) => Err(format!("网络错误: {e}")),
            })
        });
    }

    pub fn draw_save_now(&mut self) {
        let Some(ed) = &mut self.draw.ed else { return };
        if !ed.dirty || ed.saving {
            return;
        }
        ed.saving = true;
        ed.dirty = false;
        let id = ed.id.clone();
        let name = ed.name.clone();
        let elements = ed.elements.clone();
        let tx = self.rx_tx();
        let base = self.api_base.clone();
        go(&self.client, &self.ctx, &tx, move |c| {
            let body = serde_json::json!({ "name": name, "width": 960, "height": 600, "elements": elements });
            let r = c
                .put(format!("{base}/api/drawing/{id}"))
                .header("Content-Type", "application/json")
                .body(body.to_string())
                .send();
            Msg::DrawSaved {
                id: id.clone(),
                res: match r {
                    Ok(resp) if resp.status().is_success() => Ok(()),
                    Ok(resp) => Err(format!("HTTP {}", resp.status())),
                    Err(e) => Err(format!("网络错误: {e}")),
                },
            }
        });
    }

    pub fn draw_delete(&mut self, id: String) {
        let tx = self.rx_tx();
        let base = self.api_base.clone();
        go(&self.client, &self.ctx, &tx, move |c| {
            let r = c.post(format!("{base}/api/drawing/{id}/delete")).send();
            Msg::DrawDeleted(match r {
                Ok(resp) if resp.status().is_success() => Ok(id.clone()),
                Ok(resp) => Err(format!("HTTP {}", resp.status())),
                Err(e) => Err(format!("网络错误: {e}")),
            })
        });
    }

    pub fn ui_draw(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("draw-bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.selectable_label(self.draw.ed.is_none(), "🎨 画廊").clicked() {
                    self.draw.ed = None;
                    self.draw_load_list();
                }
                let ed_snap = self.draw.ed.as_ref().map(|ed| (ed.name.clone(), !ed.elements.is_empty(), ed.dirty, ed.saving));
                if let Some((name_v, can_undo, dirty, saving)) = ed_snap {
                    ui.separator();
                    ui.label("名称");
                    let mut name = name_v;
                    if ui.add(egui::TextEdit::singleline(&mut name).desired_width(160.0)).changed() {
                        if let Some(ed) = &mut self.draw.ed {
                            ed.name = name;
                            ed.mark_edit();
                        }
                    }
                    if ui.button("↩ 撤销").clicked() && can_undo {
                        if let Some(ed) = &mut self.draw.ed {
                            ed.elements.pop();
                            ed.mark_edit();
                        }
                    }
                    if ui.button("🗑 清空").clicked() {
                        if let Some(ed) = &mut self.draw.ed {
                            ed.elements.clear();
                            ed.mark_edit();
                        }
                    }
                    if dirty {
                        ui.colored_label(egui::Color32::from_rgb(230, 160, 40), "● 未保存");
                    } else if saving {
                        ui.weak("保存中…");
                    } else {
                        ui.weak("已保存");
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if !self.draw.status.is_empty() {
                        ui.label(egui::RichText::new(&self.draw.status).weak().size(12.0));
                    }
                });
            });
        });

        if self.draw.ed.is_some() {
            self.ui_draw_editor(ctx);
        } else {
            self.ui_draw_gallery(ctx);
        }

        // 弹窗:新建画板
        if self.draw.new_dialog {
            let mut open = true;
            egui::Window::new("新建画板")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("名称");
                        ui.add(egui::TextEdit::singleline(&mut self.draw.new_name).desired_width(200.0));
                    });
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button("创建").clicked() && !self.draw.new_name.trim().is_empty() {
                            let id = crate::new_note_id();
                            let name = self.draw.new_name.trim().to_string();
                            self.draw.new_dialog = false;
                            self.draw.new_name.clear();
                            self.draw_open(id.clone());
                            if let Some(ed) = &mut self.draw.ed {
                                ed.name = name;
                                ed.dirty = true;
                            }
                            let _ = id;
                        }
                        if ui.button("取消").clicked() {
                            self.draw.new_dialog = false;
                        }
                    });
                });
            if !open {
                self.draw.new_dialog = false;
            }
        }

        // 弹窗:删除确认
        if let Some(did) = self.draw.confirm_delete.clone() {
            let mut open = true;
            egui::Window::new("确认删除")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.label(format!("删除画板 {did}?"));
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button("删除").clicked() {
                            self.draw_delete(did.clone());
                            self.draw.confirm_delete = None;
                        }
                        if ui.button("取消").clicked() {
                            self.draw.confirm_delete = None;
                        }
                    });
                });
            if !open {
                self.draw.confirm_delete = None;
            }
        }

        // 弹窗:文字输入
        if let Some((pos, _)) = self.draw.text_dialog.clone() {
            let mut open = true;
            let mut close = false;
            egui::Window::new("文字")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .open(&mut open)
                .show(ctx, |ui| {
                    if let Some((_, text)) = &mut self.draw.text_dialog {
                        ui.add(egui::TextEdit::singleline(text).desired_width(240.0).hint_text("输入文字"));
                    }
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button("确定").clicked() {
                            if let Some((pos, text)) = self.draw.text_dialog.take() {
                                let text: String = text.trim().chars().take(200).collect();
                                if !text.is_empty() {
                                    if let Some(ed) = &mut self.draw.ed {
                                        ed.elements.push(DrawEl::Text {
                                            color: self.draw.color.clone(),
                                            x: pos[0],
                                            y: pos[1],
                                            font_size: 24.0,
                                            text,
                                        });
                                        ed.mark_edit();
                                    }
                                }
                            }
                            close = true;
                        }
                        if ui.button("取消").clicked() {
                            self.draw.text_dialog = None;
                            close = true;
                        }
                    });
                });
            if !open || close {
                self.draw.text_dialog = None;
            }
        }
    }

    fn ui_draw_gallery(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("＋ 新建画板").clicked() {
                    self.draw.new_dialog = true;
                    self.draw.new_name = format!("未命名 {}", chrono_now());
                }
                if ui.button("🔄 刷新").clicked() {
                    self.draw_load_list();
                }
                if self.draw.loading {
                    ui.spinner();
                }
            });
            ui.separator();
            egui::ScrollArea::vertical().show(ui, |ui| {
                let list = self.draw.list.clone();
                for m in &list {
                    ui.horizontal(|ui| {
                        let title = if m.name.is_empty() { "(未命名)".to_string() } else { m.name.clone() };
                        if ui.button(format!("🎨 {title}")).on_hover_text(&m.updated_at).clicked() {
                            self.draw_open(m.id.clone());
                        }
                        ui.weak(&m.updated_at);
                        if ui.small_button("✕").clicked() {
                            self.draw.confirm_delete = Some(m.id.clone());
                        }
                    });
                }
                if list.is_empty() && !self.draw.loading {
                    ui.weak("还没有画板,点「＋ 新建画板」开始");
                }
            });
        });
    }

    fn ui_draw_editor(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("draw-tools").resizable(false).show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                for (t, label, tip) in TOOLS {
                    if ui.selectable_label(self.draw.tool == t, egui::RichText::new(label).size(16.0)).on_hover_text(tip).clicked() {
                        self.draw.tool = t;
                    }
                }
                ui.separator();
                for c in COLORS {
                    let selected = self.draw.color == c;
                    let (rect, resp) = ui.allocate_exact_size(egui::Vec2::splat(20.0), Sense::click());
                    ui.painter().rect_filled(rect, CornerRadius::same(4), color_of(c));
                    if selected {
                        ui.painter().rect_stroke(rect, CornerRadius::same(4), Stroke::new(2.0, egui::Color32::from_rgb(79, 70, 229)), StrokeKind::Inside);
                    }
                    if resp.clicked() {
                        self.draw.color = c.to_string();
                    }
                    ui.add_space(2.0);
                }
                ui.separator();
                for w in WIDTHS {
                    if ui.selectable_label((self.draw.width - w).abs() < 0.1, egui::RichText::new("●").size(8.0 + w as f32 * 2.0)).on_hover_text(format!("线宽 {w}")).clicked() {
                        self.draw.width = w;
                    }
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            let Some(ed) = &mut self.draw.ed else { return };
            let (response, painter) =
                ui.allocate_painter(Vec2::new(ui.available_width(), ui.available_height()), Sense::click_and_drag());

            // 画布白底(等比缩放居中)
            let scale = (response.rect.width() / CW).min(response.rect.height() / CH);
            let cw = CW * scale;
            let ch = CH * scale;
            let origin = Pos2::new(
                response.rect.left() + (response.rect.width() - cw) / 2.0,
                response.rect.top() + (response.rect.height() - ch) / 2.0,
            );
            let canvas = Rect::from_min_size(origin, Vec2::new(cw, ch));
            painter.rect_filled(canvas, CornerRadius::ZERO, Color32::WHITE);
            painter.rect_stroke(canvas, CornerRadius::ZERO, Stroke::new(1.0, Color32::from_gray(200)), StrokeKind::Inside);

            let to_canvas = |p: Pos2| -> Option<[f64; 2]> {
                if canvas.contains(p) {
                    Some([((p.x - origin.x) / scale) as f64, ((p.y - origin.y) / scale) as f64])
                } else {
                    None
                }
            };
            let to_screen = |q: [f64; 2]| Pos2::new(origin.x + q[0] as f32 * scale, origin.y + q[1] as f32 * scale);

            // 回放已完成元素(裁剪到画布内,并按 origin/scale 映射)
            let cp = painter.with_clip_rect(canvas);
            for el in &ed.elements {
                paint_el(&cp, el, origin, scale);
            }
            if let Some(ip) = &ed.in_progress {
                paint_el(&cp, ip, origin, scale);
            }
            let _ = to_screen;

            // 交互
            let ptr = response.interact_pointer_pos();
            match self.draw.tool {
                DrawTool::Text => {
                    if response.clicked() {
                        if let (Some(p), Some(q)) = (ptr, ptr.and_then(to_canvas)) {
                            let _ = p;
                            self.draw.text_dialog = Some((q, String::new()));
                        }
                    }
                }
                DrawTool::Pen | DrawTool::Eraser => {
                    if response.drag_started() {
                        if let Some(q) = ptr.and_then(to_canvas) {
                            ed.drawing = true;
                            let color = self.draw.color.clone();
                            let width = self.draw.width;
                            ed.in_progress = Some(if self.draw.tool == DrawTool::Pen {
                                DrawEl::Pen { color, width, points: vec![q] }
                            } else {
                                DrawEl::Eraser { color, width, points: vec![q] }
                            });
                        }
                    }
                    if response.dragged() && ed.drawing {
                        if let (Some(ip), Some(q)) = (&mut ed.in_progress, ptr.and_then(to_canvas)) {
                            match ip {
                                DrawEl::Pen { points, .. } | DrawEl::Eraser { points, .. } => points.push(q),
                                _ => {}
                            }
                        }
                    }
                    if response.drag_released() && ed.drawing {
                        ed.drawing = false;
                        if let Some(ip) = ed.in_progress.take() {
                            ed.elements.push(ip);
                            ed.mark_edit();
                        }
                    }
                }
                DrawTool::Line | DrawTool::Rect | DrawTool::Ellipse => {
                    if response.drag_started() {
                        if let Some(q) = ptr.and_then(to_canvas) {
                            ed.drawing = true;
                            ed.start = q;
                            let color = self.draw.color.clone();
                            let width = self.draw.width;
                            let (x0, y0) = (q[0], q[1]);
                            let mk = |kind: &str| -> DrawEl {
                                match kind {
                                    "line" => DrawEl::Line { color: color.clone(), width, x0, y0, x1: x0, y1: y0 },
                                    "rect" => DrawEl::Rect { color: color.clone(), width, x0, y0, x1: x0, y1: y0 },
                                    _ => DrawEl::Ellipse { color: color.clone(), width, x0, y0, x1: x0, y1: y0 },
                                }
                            };
                            ed.in_progress = Some(match self.draw.tool {
                                DrawTool::Line => mk("line"),
                                DrawTool::Rect => mk("rect"),
                                _ => mk("ellipse"),
                            });
                        }
                    }
                    if response.dragged() && ed.drawing {
                        if let (Some(ip), Some(q)) = (&mut ed.in_progress, ptr.and_then(to_canvas)) {
                            match ip {
                                DrawEl::Line { x1, y1, .. }
                                | DrawEl::Rect { x1, y1, .. }
                                | DrawEl::Ellipse { x1, y1, .. } => {
                                    *x1 = q[0];
                                    *y1 = q[1];
                                }
                                _ => {}
                            }
                        }
                    }
                    if response.drag_released() && ed.drawing {
                        ed.drawing = false;
                        if let Some(ip) = ed.in_progress.take() {
                            ed.elements.push(ip);
                            ed.mark_edit();
                        }
                    }
                }
            }
            ctx.request_repaint();
        });
    }
}

/// 本地时间串(画廊新建命名用)。
fn chrono_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    let days = now.div_euclid(86400);
    let rem = now.rem_euclid(86400);
    format!("{:02}:{:02}", rem / 3600, (rem % 3600) / 60)
}
