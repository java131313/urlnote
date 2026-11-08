//! 自绘文本输入框(TextField View)——GPUI core 不带输入组件,这里实现一个:
//! 单行/多行、placeholder、密码掩码、IME 中文输入(EntityInputHandler)、
//! 鼠标点选/拖选/双击选词、方向键移动、剪贴板、滚动。文本状态放
//! Arc<Mutex<FieldInner>>,渲染(canvas prepaint/paint)与事件、IME 处理器共用。

use std::ops::Range;
use std::sync::{Arc, Mutex};

use gpui::prelude::*;
use gpui::{
    BorderStyle, Bounds, Context, ElementInputHandler, Entity, EntityInputHandler, FocusHandle,
    Font, Hsla, MouseButton, Pixels, Point, Render, SharedString, Styled, TextRun, UTF16Selection,
    Window, WrappedLine, canvas, div, font, hsla, point, px, rgb,
};

const PAD: f32 = 6.0;
const CARET_W: f32 = 2.0;

pub struct FieldInner {
    pub text: String,
    pub cursor: usize,            // 字节偏移(real)
    pub anchor: usize,            // 选区锚点(== cursor 表示无选区)
    pub scroll: Pixels,           // 纵向滚动(多行)
    pub marked: Option<Range<usize>>, // IME 预编辑区(real 字节)
    pub font_size: Pixels,
    pub drag_select: bool,
    pub geom: Option<FieldGeom>,
}

impl FieldInner {
    fn new(font_size: Pixels) -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            anchor: 0,
            scroll: px(0.0),
            marked: None,
            font_size,
            drag_select: false,
            geom: None,
        }
    }
    fn sel_range(&self) -> Option<Range<usize>> {
        if self.anchor != self.cursor {
            Some(self.anchor.min(self.cursor)..self.anchor.max(self.cursor))
        } else {
            None
        }
    }
}

#[derive(Clone)]
pub struct FieldGeom {
    pub bounds: Bounds<Pixels>,
    /// 第 0 行文本的绘制原点(窗口坐标,含滚动)。
    pub text_origin: Point<Pixels>,
    pub lines: Vec<WrappedLine>,
    /// 每个逻辑行在 display 文本中的字节起始。
    pub starts: Vec<usize>,
    /// 每个逻辑行的显示行数(wrap 后)。
    pub rows: Vec<usize>,
    pub line_height: Pixels,
    /// 每个逻辑行的 y 偏移(行顶部)。
    pub offsets: Vec<Pixels>,
    pub display_len: usize,
}

pub struct TextField {
    pub focus: FocusHandle,
    pub inner: Arc<Mutex<FieldInner>>,
    pub multiline: bool,
    pub password: bool,
    pub mono: bool,
    pub placeholder: SharedString,
    /// 回车(单行)回调。
    pub on_enter: Option<Arc<dyn Fn(&mut Window, &mut Context<TextField>) + Send + Sync>>,
    /// 方向键/Tab/Esc 等"特殊键"透传给父级(词典面板导航等)。key 为 keystroke.key。
    pub on_special: Option<Arc<dyn Fn(&str, &mut Window, &mut Context<TextField>) + Send + Sync>>,
}

impl TextField {
    pub fn new(cx: &mut Context<TextField>) -> Self {
        Self::with_opts(cx, false, false, false, px(14.0), "")
    }

    pub fn multiline(cx: &mut Context<TextField>) -> Self {
        Self::with_opts(cx, true, false, false, px(14.0), "")
    }

    pub fn with_opts(
        cx: &mut Context<TextField>,
        multiline: bool,
        password: bool,
        mono: bool,
        font_size: Pixels,
        placeholder: impl Into<SharedString>,
    ) -> Self {
        Self {
            focus: cx.focus_handle(),
            inner: Arc::new(Mutex::new(FieldInner::new(font_size))),
            multiline,
            password,
            mono,
            placeholder: placeholder.into(),
            on_enter: None,
            on_special: None,
        }
    }

    pub fn text(&self) -> String {
        self.inner.lock().unwrap().text.clone()
    }

    pub fn set_text(&mut self, text: String, cx: &mut Context<TextField>) {
        let mut g = self.inner.lock().unwrap();
        g.text = text;
        g.cursor = 0;
        g.anchor = 0;
        g.scroll = px(0.0);
        g.marked = None;
        drop(g);
        cx.notify();
    }

    pub fn set_font_size(&mut self, size: Pixels, cx: &mut Context<TextField>) {
        self.inner.lock().unwrap().font_size = size;
        cx.notify();
    }

    pub fn cursor(&self) -> usize {
        self.inner.lock().unwrap().cursor
    }

    pub fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// 上一行/下一行移动光标(词典面板 ↑↓ 透传时用)。
    pub fn arrow_row(&mut self, up: bool, shift: bool, cx: &mut Context<TextField>) {
        let g = self.inner.lock().unwrap();
        let Some(geom) = g.geom.clone() else { return };
        let d = self.real_to_display(&g.text, g.cursor);
        drop(g);
        if let Some((line_ix, _local, pos)) = self.display_pos(&geom, d) {
            let lh = geom.line_height;
            let cur_y = geom.offsets[line_ix] + pos.y;
            let target_y = if up { (cur_y - lh).max(px(0.0)) } else { cur_y + lh };
            // 文本空间 (pos.x, target_y) → hit_test_geom 的 y_in = target_y + PAD
            let d2 = self.hit_test_geom(&geom, target_y + px(PAD), pos.x);
            let real = self.display_to_real(&self.inner.lock().unwrap().text, d2);
            self.move_cursor(real, shift);
            cx.notify();
        } else if up {
            self.move_cursor(0, shift);
            cx.notify();
        }
    }

    /// 用选中词替换光标前的锚词(词典接受)。
    pub fn accept_word(&mut self, word: &str, cx: &mut Context<TextField>) {
        let mut g = self.inner.lock().unwrap();
        let idx = g.cursor.min(g.text.len());
        let start = crate::state::word_start_before(&g.text, idx);
        g.text.replace_range(start..idx, word);
        g.cursor = start + word.len();
        g.anchor = g.cursor;
        g.marked = None;
        drop(g);
        cx.notify();
    }

    pub fn focused(&self, window: &Window) -> bool {
        self.focus.is_focused(window)
    }

    // ---- 显示文本映射(密码掩码) ----

    fn to_display(&self, text: &str) -> String {
        if self.password {
            "•".repeat(text.chars().count())
        } else {
            text.to_string()
        }
    }

    /// real 字节偏移 → display 字节偏移。
    fn real_to_display(&self, text: &str, real: usize) -> usize {
        if self.password {
            text[..real].chars().count() * 3
        } else {
            real
        }
    }

    /// display 字节偏移 → real 字节偏移。
    fn display_to_real(&self, text: &str, display: usize) -> usize {
        if self.password {
            let n_chars = display / 3;
            text.char_indices().nth(n_chars).map(|(i, _)| i).unwrap_or(text.len())
        } else {
            display
        }
    }

    // ---- 布局 ----

    fn font(&self) -> Font {
        if self.mono {
            font("Menlo")
        } else {
            font(".SystemUIFont")
        }
    }

    fn shape(
        &self,
        display: &str,
        font_size: Pixels,
        wrap_width: Option<Pixels>,
        text_color: Hsla,
        window: &mut Window,
    ) -> Vec<WrappedLine> {
        let run = TextRun {
            len: display.len(),
            font: self.font(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        window
            .text_system()
            .shape_text(display.to_string().into(), font_size, &[run], wrap_width, None)
            .map(|v| v.into_vec())
            .unwrap_or_default()
    }

    /// 计算几何(渲染与命中共用)。scroll_adjust: 是否据光标调整滚动。
    fn compute_geom(
        &self,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        scroll_adjust: bool,
    ) -> FieldGeom {
        let (display, cursor_d, font_size, mut scroll) = {
            let g = self.inner.lock().unwrap();
            (self.to_display(&g.text), self.real_to_display(&g.text, g.cursor), g.font_size, g.scroll)
        };
        let lh = font_size * 1.5;
        let text_w = (bounds.size.width - px(PAD * 2.0)).max(px(20.0));
        let wrap = if self.multiline { Some(text_w) } else { None };
        let lines = self.shape(&display, font_size, wrap, hsla(0.0, 0.0, 0.13, 1.0), window);

        let mut starts = Vec::with_capacity(lines.len());
        let mut rows = Vec::with_capacity(lines.len());
        let mut offsets = Vec::with_capacity(lines.len());
        let mut off = px(0.0);
        let mut byte_pos = 0usize;
        for line in &lines {
            starts.push(byte_pos);
            let r = line.wrap_boundaries().len() + 1;
            rows.push(r);
            offsets.push(off);
            off += lh * r as f32;
            byte_pos += line.len() + 1;
        }
        let display_len = display.len();

        if scroll_adjust {
            // 光标可见性
            let max_scroll = (off - (bounds.size.height - px(PAD * 2.0))).max(px(0.0));
            let caret_y = self.caret_local_y(&lines, &starts, &rows, cursor_d, lh);
            if caret_y < scroll {
                scroll = caret_y.max(px(0.0));
            } else if caret_y + lh > scroll + (bounds.size.height - px(PAD * 2.0)) {
                scroll = (caret_y + lh - (bounds.size.height - px(PAD * 2.0))).max(px(0.0));
            }
            scroll = scroll.max(px(0.0)).min(max_scroll);
            self.inner.lock().unwrap().scroll = scroll;
        }

        FieldGeom {
            bounds,
            text_origin: point(bounds.origin.x + px(PAD), bounds.origin.y + px(PAD) - scroll),
            lines,
            starts,
            rows,
            line_height: lh,
            offsets,
            display_len,
        }
    }

    /// 光标(display 字节)在其逻辑行内的 y(相对行顶部)。
    fn caret_local_y(
        &self,
        lines: &[WrappedLine],
        starts: &[usize],
        rows: &[usize],
        cursor_d: usize,
        lh: Pixels,
    ) -> Pixels {
        let mut y = px(0.0);
        for (i, line) in lines.iter().enumerate() {
            let local = cursor_d.saturating_sub(starts[i]).min(line.len());
            let pos = line.position_for_index(local, lh).unwrap_or(point(px(0.0), px(0.0)));
            y = self.geom_base_for(i, starts, rows, lh) + pos.y;
            if cursor_d <= starts[i] + line.len() {
                break;
            }
        }
        y
    }

    fn geom_base_for(&self, line_ix: usize, _starts: &[usize], rows: &[usize], lh: Pixels) -> Pixels {
        let mut y = px(0.0);
        for r in rows.iter().take(line_ix) {
            y += lh * *r as f32;
        }
        y
    }

    /// 命中测试:元素内坐标 → real 字节偏移。
    fn hit_test(&self, local: Point<Pixels>) -> usize {
        let g = self.inner.lock().unwrap();
        let Some(geom) = g.geom.clone() else { return 0 };
        let scroll = g.scroll;
        drop(g);
        // y_in = 元素 y + 滚动 = 文本空间 y + PAD
        self.hit_test_geom(&geom, local.y + scroll, local.x - px(PAD))
    }

    fn hit_test_geom(&self, geom: &FieldGeom, y_in: Pixels, x_in: Pixels) -> usize {
        let lh = geom.line_height;
        let mut acc = px(0.0);
        for (i, line) in geom.lines.iter().enumerate() {
            let h = lh * geom.rows[i] as f32;
            if y_in < acc + h || i == geom.lines.len() - 1 {
                let ly = (y_in - acc).max(px(0.0));
                let local = point(x_in.max(px(0.0)), ly);
                let idx = line
                    .index_for_position(local, lh)
                    .unwrap_or(0)
                    .min(line.len());
                return geom.starts[i] + idx;
            }
            acc += h;
        }
        geom.display_len
    }

    /// display 字节 → (行号, 行内字节, x, y相对文本原点)。
    fn display_pos(&self, geom: &FieldGeom, d: usize) -> Option<(usize, usize, Point<Pixels>)> {
        for (i, line) in geom.lines.iter().enumerate() {
            if d <= geom.starts[i] + line.len() || i == geom.lines.len() - 1 {
                let local = d.saturating_sub(geom.starts[i]).min(line.len());
                let pos = line.position_for_index(local, geom.line_height)?;
                return Some((i, local, pos));
            }
        }
        None
    }

    // ---- 编辑操作 ----

    fn replace_range(&self, range: Range<usize>, text: &str) {
        let mut g = self.inner.lock().unwrap();
        let end = (range.end).min(g.text.len());
        let start = range.start.min(end);
        g.text.replace_range(start..end, text);
        g.cursor = start + text.len();
        g.anchor = g.cursor;
        g.marked = None;
    }

    fn delete_selection_or(&self, backwards: bool) {
        let mut g = self.inner.lock().unwrap();
        if let Some(sel) = g.sel_range() {
            g.text.replace_range(sel.clone(), "");
            g.cursor = sel.start;
            g.anchor = g.cursor;
            g.marked = None;
        } else if backwards {
            if g.cursor > 0 {
                let prev = g.text[..g.cursor].chars().next_back().unwrap();
                let start = g.cursor - prev.len_utf8();
                let cur = g.cursor;
                g.text.replace_range(start..cur, "");
                g.cursor = start;
                g.anchor = g.cursor;
            }
        } else {
            if g.cursor < g.text.len() {
                let next = g.text[g.cursor..].chars().next().unwrap();
                let end = g.cursor + next.len_utf8();
                let cur = g.cursor;
                g.text.replace_range(cur..end, "");
                g.anchor = g.cursor;
            }
        }
    }

    fn insert_at_cursor(&self, text: &str) {
        let mut g = self.inner.lock().unwrap();
        let range = g.sel_range().unwrap_or(g.cursor..g.cursor);
        g.text.replace_range(range.clone(), text);
        g.cursor = range.start + text.len();
        g.anchor = g.cursor;
        g.marked = None;
    }

    fn move_cursor(&self, new: usize, extend: bool) {
        let mut g = self.inner.lock().unwrap();
        let new = new.min(g.text.len());
        if !extend {
            g.anchor = new;
        }
        g.cursor = new;
    }

    fn select_all(&self) {
        let mut g = self.inner.lock().unwrap();
        g.anchor = 0;
        g.cursor = g.text.len();
    }

    fn selected_text(&self) -> Option<String> {
        let g = self.inner.lock().unwrap();
        g.sel_range().map(|r| g.text[r].to_string())
    }

    fn line_bounds_around(&self, pos: usize) -> (usize, usize) {
        let g = self.inner.lock().unwrap();
        let text = &g.text;
        let mut start = pos.min(text.len());
        while start > 0 && !text.is_char_boundary(start) {
            start -= 1;
        }
        let mut s = start;
        while s > 0 {
            if text.as_bytes()[s - 1] == b'\n' {
                break;
            }
            s -= 1;
        }
        let mut e = start;
        while e < text.len() && text.as_bytes()[e] != b'\n' {
            e += 1;
        }
        (s, e)
    }

    fn word_bounds_at(&self, pos: usize) -> (usize, usize) {
        let g = self.inner.lock().unwrap();
        let text = &g.text;
        if text.is_empty() {
            return (0, 0);
        }
        let mut p = pos.min(text.len());
        while p > 0 && !text.is_char_boundary(p) {
            p -= 1;
        }
        let mut s = p;
        while s > 0 {
            let c = text[..s].chars().next_back().unwrap();
            if !(c.is_alphanumeric() || (0x4E00..=0x9FFF).contains(&(c as u32))) {
                break;
            }
            s -= c.len_utf8();
        }
        let mut e = p;
        while e < text.len() {
            let c = text[e..].chars().next().unwrap();
            if !(c.is_alphanumeric() || (0x4E00..=0x9FFF).contains(&(c as u32))) {
                break;
            }
            e += c.len_utf8();
        }
        (s, e)
    }

    // ---- 键盘 ----

    fn handle_key(&mut self, keystroke: &gpui::Keystroke, window: &mut Window, cx: &mut Context<TextField>) {
        let m = keystroke.modifiers;
        let shift = m.shift;
        let cmd = m.platform;
        let key = keystroke.key.as_str();

        match key {
            "enter" => {
                if self.multiline {
                    self.insert_at_cursor("\n");
                    cx.notify();
                } else if let Some(cb) = self.on_enter.clone() {
                    cb(window, cx);
                }
                return;
            }
            "tab" => {
                if let Some(cb) = self.on_special.clone() {
                    cb("tab", window, cx);
                }
                return;
            }
            "escape" => {
                if let Some(cb) = self.on_special.clone() {
                    cb("escape", window, cx);
                }
                return;
            }
            _ => {}
        }

        if cmd && !shift {
            match key {
                "a" => {
                    self.select_all();
                    cx.notify();
                    return;
                }
                "c" => {
                    if let Some(t) = self.selected_text() {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(t));
                    }
                    return;
                }
                "x" => {
                    if let Some(t) = self.selected_text() {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(t));
                        self.delete_selection_or(true);
                        cx.notify();
                    }
                    return;
                }
                "v" => {
                    let text = cx
                        .read_from_clipboard()
                        .and_then(|item| item.text())
                        .map(|s| {
                            if self.multiline {
                                s.replace("\r\n", "\n")
                            } else {
                                s.replace(['\n', '\r'], " ")
                            }
                        });
                    if let Some(text) = text {
                        self.insert_at_cursor(&text);
                        cx.notify();
                    }
                    return;
                }
                _ => {}
            }
        }

        if cmd && key == "left" {
            let (s, _) = self.line_bounds_around(self.inner.lock().unwrap().cursor);
            self.move_cursor(s, shift);
            cx.notify();
            return;
        }
        if cmd && key == "right" {
            let (_, e) = self.line_bounds_around(self.inner.lock().unwrap().cursor);
            self.move_cursor(e, shift);
            cx.notify();
            return;
        }

        match key {
            "backspace" => {
                self.delete_selection_or(true);
                cx.notify();
            }
            "delete" => {
                self.delete_selection_or(false);
                cx.notify();
            }
            "left" => {
                let cur = self.inner.lock().unwrap().cursor;
                let target = if m.alt {
                    let (s, _) = self.word_bounds_at(cur);
                    if s == cur {
                        prev_char(&self.inner.lock().unwrap().text, cur).unwrap_or(0)
                    } else {
                        s
                    }
                } else {
                    prev_char(&self.inner.lock().unwrap().text, cur).unwrap_or(cur)
                };
                self.move_cursor(target, shift);
                cx.notify();
            }
            "right" => {
                let cur = self.inner.lock().unwrap().cursor;
                let text = self.inner.lock().unwrap().text.clone();
                let target = if m.alt {
                    let (_, e) = self.word_bounds_at(cur);
                    if e == cur {
                        next_char(&text, cur).unwrap_or(text.len())
                    } else {
                        e
                    }
                } else {
                    next_char(&text, cur).unwrap_or(cur)
                };
                self.move_cursor(target, shift);
                cx.notify();
            }
            "up" | "down" => {
                self.arrow_row(key == "up", shift, cx);
            }
            "home" => {
                let cur = self.inner.lock().unwrap().cursor;
                let (s, _) = self.line_bounds_around(cur);
                self.move_cursor(if cmd { 0 } else { s }, shift);
                cx.notify();
            }
            "end" => {
                let cur = self.inner.lock().unwrap().cursor;
                let (_, e) = self.line_bounds_around(cur);
                let end = if cmd { self.inner.lock().unwrap().text.len() } else { e };
                self.move_cursor(end, shift);
                cx.notify();
            }
            _ => {}
        }
    }
}

fn prev_char(text: &str, pos: usize) -> Option<usize> {
    if pos == 0 {
        return None;
    }
    text[..pos].chars().next_back().map(|c| pos - c.len_utf8())
}

fn next_char(text: &str, pos: usize) -> Option<usize> {
    if pos >= text.len() {
        return None;
    }
    text[pos..].chars().next().map(|c| pos + c.len_utf8())
}

// ---- UTF-16 换算(IME 协议) ----

fn byte_to_utf16(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())].chars().map(|c| c.len_utf16()).sum()
}

fn utf16_to_byte(text: &str, u16ix: usize) -> usize {
    let mut acc = 0usize;
    for (b, c) in text.char_indices() {
        if acc >= u16ix {
            return b;
        }
        acc += c.len_utf16();
    }
    text.len()
}

// ---- IME ----

impl EntityInputHandler for TextField {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let g = self.inner.lock().unwrap();
        let s = utf16_to_byte(&g.text, range.start);
        let e = utf16_to_byte(&g.text, range.end).max(s);
        let e = e.min(g.text.len());
        let s = s.min(e);
        *adjusted_range = Some(range);
        Some(g.text[s..e].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let g = self.inner.lock().unwrap();
        if g.cursor == g.anchor && g.marked.is_none() {
            let pos = byte_to_utf16(&g.text, g.cursor);
            return Some(UTF16Selection { range: pos..pos, reversed: false });
        }
        let a = byte_to_utf16(&g.text, g.anchor);
        let c = byte_to_utf16(&g.text, g.cursor);
        Some(UTF16Selection {
            range: a.min(c)..a.max(c),
            reversed: g.anchor > g.cursor,
        })
    }

    fn marked_text_range(&self, _window: &mut Window, _cx: &mut Context<Self>) -> Option<Range<usize>> {
        let g = self.inner.lock().unwrap();
        g.marked
            .as_ref()
            .map(|r| byte_to_utf16(&g.text, r.start)..byte_to_utf16(&g.text, r.end))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.inner.lock().unwrap().marked = None;
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let g = self.inner.lock().unwrap();
        let brange = match range {
            Some(r) => utf16_to_byte(&g.text, r.start)..utf16_to_byte(&g.text, r.end),
            None => g.sel_range().unwrap_or(g.cursor..g.cursor),
        };
        let brange = brange.start.min(g.text.len())..brange.end.min(g.text.len());
        drop(g);
        self.replace_range(brange, text);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut g = self.inner.lock().unwrap();
        let brange = match range {
            Some(r) => utf16_to_byte(&g.text, r.start)..utf16_to_byte(&g.text, r.end),
            None => g.sel_range().unwrap_or(g.cursor..g.cursor),
        };
        let brange = brange.start.min(g.text.len())..brange.end.min(g.text.len());
        let start = brange.start;
        g.text.replace_range(brange, new_text);
        let marked_start = start;
        let marked_end = start + new_text.len();
        g.marked = Some(marked_start..marked_end);
        if let Some(sel) = new_selected_range {
            let s = utf16_to_byte(new_text, sel.start);
            let e = utf16_to_byte(new_text, sel.end);
            g.cursor = (start + e).min(g.text.len());
            g.anchor = (start + s).min(g.text.len());
        } else {
            g.cursor = marked_end;
            g.anchor = g.cursor;
        }
        drop(g);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let g = self.inner.lock().unwrap();
        let geom = g.geom.clone()?;
        let text = g.text.clone();
        drop(g);
        let display = self.to_display(&text);
        let s = self.real_to_display(&text, utf16_to_byte(&text, range_utf16.start));
        let e = self.real_to_display(&text, utf16_to_byte(&text, range_utf16.end).max(utf16_to_byte(&text, range_utf16.start)));
        let (s_line, _, s_pos) = self.display_pos(&geom, s.min(display.len()))?;
        let (e_line, _, e_pos) = self.display_pos(&geom, e.min(display.len()))?;
        let lh = geom.line_height;
        let origin = geom.text_origin;
        let local_start = point(origin.x + s_pos.x, origin.y + geom.offsets[s_line] + s_pos.y);
        let local_end = point(origin.x + e_pos.x, origin.y + geom.offsets[e_line] + e_pos.y);
        if s_line == e_line && s_pos.y == e_pos.y {
            let x0 = local_start.x.min(local_end.x);
            let x1 = local_start.x.max(local_end.x).max(x0 + px(4.0));
            Some(Bounds::new(
                point(element_bounds.origin.x.max(x0), local_start.y),
                gpui::size(x1 - x0, lh),
            ))
        } else {
            // 跨行:取元素宽度内的并集
            let y0 = local_start.y.min(local_end.y);
            let y1 = (local_start.y.max(local_end.y) + lh).min(element_bounds.bottom());
            Some(Bounds::new(
                point(element_bounds.origin.x, y0),
                gpui::size(element_bounds.size.width, (y1 - y0).max(lh)),
            ))
        }
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let g = self.inner.lock().unwrap();
        let geom = g.geom.clone()?;
        let text = g.text.clone();
        let bounds = geom.bounds;
        let scroll = g.scroll;
        drop(g);
        // 窗口坐标 → 元素内坐标 → 文本坐标
        let lx = point.x - bounds.origin.x;
        let ly = point.y - bounds.origin.y + scroll;
        let d = self.hit_test_geom(&geom, ly, lx - px(PAD));
        Some(byte_to_utf16(&text, self.display_to_real(&text, d)))
    }
}

// ---- 渲染 ----

impl Render for TextField {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        let focused = self.focus.is_focused(window);
        let multiline = self.multiline;
        let placeholder = self.placeholder.clone();
        let inner = self.inner.clone();
        let password = self.password;
        let mono = self.mono;
        let focus = self.focus.clone();
        let entity = cx.entity().clone();

        let border_color = if focused { rgb(0x4f46e5) } else { rgb(0xd4d4d8) };

        let mut el = div()
            .track_focus(&self.focus)
            .bg(rgb(0xffffff))
            .border_1()
            .border_color(border_color)
            .rounded_md()
            .px(px(PAD))
            .py(px(PAD))
            .cursor(gpui::CursorStyle::IBeam)
            .overflow_hidden()
            .on_mouse_down(MouseButton::Left, {
                let ent = entity.clone();
                move |event, window, cx| {
                    ent.update(cx, |field: &mut TextField, cx| {
                        field.focus.focus(window);
                        let hit = field.hit_test(event.position);
                        {
                            let mut g = field.inner.lock().unwrap();
                            if event.click_count >= 2 {
                                let (s, e) = field.word_bounds_at(hit);
                                g.anchor = s;
                                g.cursor = e;
                            } else {
                                g.cursor = hit;
                                g.anchor = hit;
                                g.drag_select = true;
                            }
                        }
                        cx.notify();
                    });
                }
            })
            .on_mouse_move({
                let ent = entity.clone();
                let inner2 = inner.clone();
                move |event, _window, cx| {
                    let dragging = inner2.lock().unwrap().drag_select;
                    if dragging {
                        ent.update(cx, |field: &mut TextField, cx| {
                            let hit = field.hit_test(event.position);
                            let mut g = field.inner.lock().unwrap();
                            g.cursor = hit;
                            cx.notify();
                        });
                    }
                }
            })
            .on_mouse_up(MouseButton::Left, {
                let inner2 = inner.clone();
                let eid = entity.entity_id();
                move |_event, _window, cx| {
                    inner2.lock().unwrap().drag_select = false;
                    cx.notify(eid);
                }
            })
            .on_scroll_wheel({
                let inner2 = inner.clone();
                let eid = entity.entity_id();
                move |event, _window, cx| {
                    if multiline {
                        let delta = event.delta.pixel_delta(px(16.0)).y;
                        let mut g = inner2.lock().unwrap();
                        g.scroll = (g.scroll + delta).max(px(0.0));
                        cx.notify(eid);
                    }
                }
            })
            .on_key_down({
                let ent = entity.clone();
                move |event, window, cx| {
                    ent.update(cx, |field: &mut TextField, cx| {
                        field.handle_key(&event.keystroke, window, cx);
                    });
                }
            })
            .child(
                // prepaint:算几何并回写 scroll/geom
                canvas(
                    {
                        let inner2 = inner.clone();
                        move |bounds, window, _cx| {
                            let display = {
                                let g = inner2.lock().unwrap();
                                if password {
                                    "•".repeat(g.text.chars().count())
                                } else {
                                    g.text.clone()
                                }
                            };
                            let (font_size, scroll) = {
                                let g = inner2.lock().unwrap();
                                (g.font_size, g.scroll)
                            };
                            let lh = font_size * 1.5;
                            let text_w = (bounds.size.width - px(PAD * 2.0)).max(px(20.0));
                            let wrap = if multiline { Some(text_w) } else { None };
                            let run = TextRun {
                                len: display.len(),
                                font: if mono { font("Menlo") } else { font(".SystemUIFont") },
                                color: hsla(0.0, 0.0, 0.13, 1.0),
                                background_color: None,
                                underline: None,
                                strikethrough: None,
                            };
                            let lines: Vec<WrappedLine> = window
                                .text_system()
                                .shape_text(display.clone().into(), font_size, &[run], wrap, None)
                                .map(|v| v.into_vec())
                                .unwrap_or_default();
                            let mut starts = Vec::with_capacity(lines.len());
                            let mut rows = Vec::with_capacity(lines.len());
                            let mut offsets = Vec::with_capacity(lines.len());
                            let mut off = px(0.0);
                            let mut byte_pos = 0usize;
                            for line in &lines {
                                starts.push(byte_pos);
                                let r = line.wrap_boundaries().len() + 1;
                                rows.push(r);
                                offsets.push(off);
                                off += lh * r as f32;
                                byte_pos += line.len() + 1;
                            }
                            // 光标可见性 → 调整 scroll
                            let (cursor_d, display_len) = {
                                let g = inner2.lock().unwrap();
                                let d = if password {
                                    g.text[..g.cursor.min(g.text.len())].chars().count() * 3
                                } else {
                                    g.cursor
                                };
                                (d, display.len())
                            };
                            let mut new_scroll = scroll;
                            {
                                let mut caret_y = px(0.0);
                                let mut found = false;
                                for (i, line) in lines.iter().enumerate() {
                                    let local = cursor_d.saturating_sub(starts[i]).min(line.len());
                                    let pos = line.position_for_index(local, lh).unwrap_or(point(px(0.0), px(0.0)));
                                    caret_y = offsets[i] + pos.y;
                                    if cursor_d <= starts[i] + line.len() {
                                        found = true;
                                        break;
                                    }
                                }
                                if !found {
                                    caret_y = (off - lh).max(px(0.0));
                                }
                                let max_scroll = (off - (bounds.size.height - px(PAD * 2.0))).max(px(0.0));
                                if caret_y < new_scroll {
                                    new_scroll = caret_y.max(px(0.0));
                                } else if caret_y + lh > new_scroll + (bounds.size.height - px(PAD * 2.0)) {
                                    new_scroll = caret_y + lh - (bounds.size.height - px(PAD * 2.0));
                                }
                                new_scroll = new_scroll.max(px(0.0)).min(max_scroll);
                            }
                            let geom = FieldGeom {
                                bounds,
                                text_origin: point(
                                    bounds.origin.x + px(PAD),
                                    bounds.origin.y + px(PAD) - new_scroll,
                                ),
                                lines: lines.clone(),
                                starts,
                                rows,
                                line_height: lh,
                                offsets,
                                display_len,
                            };
                            {
                                let mut g = inner2.lock().unwrap();
                                g.scroll = new_scroll;
                                g.geom = Some(geom.clone());
                            }
                            geom
                        }
                    },
                    move |bounds, geom, window, cx| {
                        let focused = focus.is_focused(window);
                        let lh = geom.line_height;
                        let origin = geom.text_origin;
                        let (text, cursor_d, anchor_d, has_text) = {
                            let g = inner.lock().unwrap();
                            let t = if password {
                                "•".repeat(g.text.chars().count())
                            } else {
                                g.text.clone()
                            };
                            let c = if password {
                                g.text[..g.cursor.min(g.text.len())].chars().count() * 3
                            } else {
                                g.cursor
                            };
                            let a = if password {
                                g.text[..g.anchor.min(g.text.len())].chars().count() * 3
                            } else {
                                g.anchor
                            };
                            (t, c, a, !g.text.is_empty())
                        };

                        if !has_text && !focused && !placeholder.is_empty() {
                            // placeholder
                            let font_size = lh / 1.5;
                            let run = TextRun {
                                len: placeholder.len(),
                                font: if mono { font("Menlo") } else { font(".SystemUIFont") },
                                color: hsla(0.0, 0.0, 0.55, 1.0),
                                background_color: None,
                                underline: None,
                                strikethrough: None,
                            };
                            if let Ok(mut lines) = window
                                .text_system()
                                .shape_text(placeholder.clone(), font_size, &[run], None, None)
                            {
                                if let Some(l) = lines.pop() {
                                    let _ = l.paint(origin, lh, gpui::TextAlign::Left, None, window, cx);
                                }
                            }
                        } else if has_text {
                            // 选区背景
                            let sel = anchor_d.min(cursor_d)..anchor_d.max(cursor_d);
                            if sel.start < sel.end {
                                let sel_color = hsla(0.62, 0.65, 0.85, 0.55);
                                for (i, line) in geom.lines.iter().enumerate() {
                                    let ls = geom.starts[i];
                                    let le = ls + line.len();
                                    if sel.end < ls || sel.start > le {
                                        continue;
                                    }
                                    // 逐显示行
                                    let row_count = geom.rows[i];
                                    for r in 0..row_count {
                                        let row_start = if r == 0 {
                                            0
                                        } else {
                                            // 用 index_for_position 找行首
                                            line.index_for_position(
                                                point(px(0.0) + px(0.5), lh * r as f32 + lh / 2.0),
                                                lh,
                                            )
                                            .unwrap_or(line.len())
                                        };
                                        let row_end = if r + 1 < row_count {
                                            line.index_for_position(
                                                point(px(0.0) + px(0.5), lh * (r + 1) as f32 + lh / 2.0),
                                                lh,
                                            )
                                            .unwrap_or(line.len())
                                        } else {
                                            line.len()
                                        };
                                        let rs = ls + row_start;
                                        let re = ls + row_end;
                                        let s = sel.start.max(rs).min(re);
                                        let e = sel.end.max(rs).min(re);
                                        if s < e {
                                            let x0 = line
                                                .position_for_index(s - ls, lh)
                                                .map(|p| p.x)
                                                .unwrap_or(px(0.0));
                                            let x1 = line
                                                .position_for_index(e - ls, lh)
                                                .map(|p| p.x)
                                                .unwrap_or(px(0.0));
                                            window.paint_quad(gpui::PaintQuad {
                                                bounds: Bounds::new(
                                                    point(origin.x + x0, origin.y + geom.offsets[i] + lh * r as f32),
                                                    gpui::size((x1 - x0).max(px(2.0)), lh),
                                                ),
                                                corner_radii: gpui::Corners::default(),
                                                background: sel_color.into(),
                                                border_widths: gpui::Edges::default(),
                                                border_color: hsla(0.0, 0.0, 0.0, 0.0),
                                                border_style: BorderStyle::default(),
                                            });
                                        }
                                    }
                                }
                            }
                            // 文本
                            for (i, line) in geom.lines.iter().enumerate() {
                                let _ = line.paint(
                                    point(origin.x, origin.y + geom.offsets[i]),
                                    lh,
                                    gpui::TextAlign::Left,
                                    None,
                                    window,
                                    cx,
                                );
                            }
                        }

                        // 光标
                        if focused && !password {
                            for (i, line) in geom.lines.iter().enumerate() {
                                let ls = geom.starts[i];
                                let le = ls + line.len();
                                if cursor_d <= le || i == geom.lines.len() - 1 {
                                    let local = cursor_d.saturating_sub(ls).min(line.len());
                                    let pos = line
                                        .position_for_index(local, lh)
                                        .unwrap_or(point(px(0.0), px(0.0)));
                                    window.paint_quad(gpui::PaintQuad {
                                        bounds: Bounds::new(
                                            point(origin.x + pos.x, origin.y + geom.offsets[i] + pos.y),
                                            gpui::size(px(CARET_W), lh),
                                        ),
                                        corner_radii: gpui::Corners::default(),
                                        background: rgb(0x1e1b4b).into(),
                                        border_widths: gpui::Edges::default(),
                                        border_color: hsla(0.0, 0.0, 0.0, 0.0),
                                        border_style: BorderStyle::default(),
                                    });
                                    break;
                                }
                            }
                        }

                        // IME 输入处理
                        let _ = bounds;
                        window.handle_input(
                            &focus,
                            ElementInputHandler::new(geom.bounds, entity.clone()),
                            cx,
                        );
                    },
                ),
            );

        if !multiline {
            el = el.h(self.inner.lock().unwrap().font_size * 1.5 + px(PAD * 2.0));
        }
        el
    }
}
