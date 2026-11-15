//! The log view: a custom-painted window over a [`LogSource`].
//!
//! It paints only the lines that fit, at most once per timer tick, from
//! buffers it keeps between frames: copying a page of text out of the source,
//! splitting it into colour runs and drawing it allocates nothing once the
//! buffers have grown. The source's lock is held only while copying.

use std::cell::{Cell, RefCell};
use std::sync::Arc;

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DC_BRUSH, DT_CENTER, DT_SINGLELINE,
    DT_VCENTER, DeleteDC, DeleteObject, DrawTextW, FillRect, FrameRect, GetStockObject, HBITMAP,
    HBRUSH, HDC, HGDIOBJ, MoveToEx, SRCCOPY, SelectObject, SetBkMode, SetDCBrushColor,
    SetTextAlign, SetTextColor, TA_NOUPDATECP, TA_UPDATECP, TRANSPARENT, TextOutW,
};
use windows::Win32::UI::Controls::SetScrollInfo;
use windows::Win32::UI::WindowsAndMessaging::{
    SB_VERT, SCROLLINFO, SIF_DISABLENOSCROLL, SIF_PAGE, SIF_POS, SIF_RANGE, WS_TABSTOP, WS_VSCROLL,
};

use super::ansi::{self, Span};
use super::log_state::LogState;
use crate::boundary::LogSource;
use crate::theme::{Fonts, Theme};
use crate::win::{Dpi, child, client_rect, copy_to_clipboard, invalidate};

/// Longest part of a line that is drawn, in UTF-16 units. Bounds the work
/// a pathological line (a megabyte of JSON) can cause per frame.
const MAX_LINE: usize = 4096;
const TAB_WIDTH: usize = 4;
const ESC: u16 = 0x1b;

/// Copies `line` into `out` ready to draw: tabs become spaces, other control
/// characters except ESC are dropped, and the length is capped.
pub fn sanitize_into(line: &str, out: &mut Vec<u16>) {
    let start = out.len();
    for unit in line.encode_utf16() {
        if out.len() - start >= MAX_LINE {
            break;
        }
        match unit {
            0x09 => out.extend(std::iter::repeat_n(u16::from(b' '), TAB_WIDTH)),
            ESC => out.push(unit),
            0..0x20 | 0x7f => {}
            _ => out.push(unit),
        }
    }
}

#[derive(Default)]
struct Frame {
    /// The visible lines, back to back.
    text: Vec<u16>,
    /// Number and end offset in `text` of each visible line.
    lines: Vec<(u64, usize)>,
    spans: Vec<Span>,
    /// Double buffer, kept between frames and dropped when the view hides.
    dc: HDC,
    bitmap: HBITMAP,
    old_bitmap: HGDIOBJ,
    size: (i32, i32),
}

impl Frame {
    fn release(&mut self) {
        if self.dc.is_invalid() {
            return;
        }
        // SAFETY: the bitmap is selected out before both are deleted.
        unsafe {
            SelectObject(self.dc, self.old_bitmap);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
        self.dc = HDC::default();
        self.bitmap = HBITMAP::default();
        self.size = (0, 0);
    }

    fn ensure(&mut self, hdc: HDC, width: i32, height: i32) {
        if !self.dc.is_invalid() && self.size == (width, height) {
            return;
        }
        self.release();
        // SAFETY: the DC and bitmap are owned here until `release`.
        unsafe {
            self.dc = CreateCompatibleDC(Some(hdc));
            self.bitmap = CreateCompatibleBitmap(hdc, width.max(1), height.max(1));
            self.old_bitmap = SelectObject(self.dc, self.bitmap.into());
        }
        self.size = (width, height);
    }
}

pub struct LogView {
    pub hwnd: HWND,
    state: RefCell<LogState>,
    source: RefCell<Option<Arc<dyn LogSource>>>,
    /// The source's generation when it was last read.
    seen: Cell<u64>,
    line_height: Cell<i32>,
    char_width: Cell<i32>,
    /// Pixels scrolled to the right.
    shift: Cell<i32>,
    /// What the scroll bar was last told: first, end, top, page.
    scrollbar: Cell<(u64, u64, u64, u64)>,
    focused: Cell<bool>,
    frame: RefCell<Frame>,
}

impl Drop for LogView {
    fn drop(&mut self) {
        self.frame.borrow_mut().release();
    }
}

impl LogView {
    pub fn create(parent: HWND, class: windows::core::PCWSTR, id: usize) -> Self {
        Self {
            hwnd: child(
                parent,
                class,
                "Log output",
                WS_TABSTOP.0 | WS_VSCROLL.0,
                0,
                id,
            ),
            state: RefCell::new(LogState::new()),
            source: RefCell::new(None),
            seen: Cell::new(u64::MAX),
            line_height: Cell::new(16),
            char_width: Cell::new(8),
            shift: Cell::new(0),
            scrollbar: Cell::new((u64::MAX, 0, 0, 0)),
            focused: Cell::new(false),
            frame: RefCell::new(Frame::default()),
        }
    }

    pub fn set_fonts(&self, fonts: &Fonts) {
        self.line_height.set(fonts.mono_line);
        self.char_width.set(fonts.mono_char);
        self.resize();
    }

    /// Shows `source` from its newest line, or nothing.
    pub fn open(&self, source: Option<Arc<dyn LogSource>>) {
        *self.source.borrow_mut() = source;
        *self.state.borrow_mut() = LogState::new();
        self.seen.set(u64::MAX);
        self.shift.set(0);
        self.resize();
        self.poll();
        invalidate(self.hwnd);
    }

    /// Lets go of the source and of every buffer.
    pub fn close(&self) {
        *self.source.borrow_mut() = None;
        *self.state.borrow_mut() = LogState::new();
        self.release();
        let mut frame = self.frame.borrow_mut();
        frame.text = Vec::new();
        frame.lines = Vec::new();
        frame.spans = Vec::new();
    }

    /// Drops the double buffer while nothing is drawn.
    pub fn release(&self) {
        self.frame.borrow_mut().release();
    }

    pub fn set_focused(&self, focused: bool) {
        self.focused.set(focused);
        invalidate(self.hwnd);
    }

    pub fn resize(&self) {
        let height = client_rect(self.hwnd).bottom;
        let page = (height / self.line_height.get().max(1)).max(1) as u64;
        self.state.borrow_mut().set_page(page);
        self.sync_scrollbar();
    }

    /// Reads the source's bounds if it changed. Returns whether it did.
    pub fn poll(&self) -> bool {
        let Some(source) = self.source.borrow().clone() else {
            return false;
        };
        let generation = source.generation();
        if self.seen.replace(generation) == generation {
            return false;
        }
        let (first, end) = source.bounds();
        self.state.borrow_mut().set_bounds(first, end);
        true
    }

    /// Tells the scroll bar where the view is, if that changed.
    pub fn sync_scrollbar(&self) {
        let (first, end, top, page) = {
            let state = self.state.borrow();
            (state.first(), state.end(), state.top(), state.page())
        };
        if self.scrollbar.replace((first, end, top, page)) == (first, end, top, page) {
            return;
        }
        let info = SCROLLINFO {
            cbSize: size_of::<SCROLLINFO>() as u32,
            fMask: SIF_RANGE | SIF_PAGE | SIF_POS | SIF_DISABLENOSCROLL,
            nMin: 0,
            nMax: (end - first).saturating_sub(1).min(i32::MAX as u64) as i32,
            nPage: page.min(u32::MAX as u64) as u32,
            nPos: (top - first).min(i32::MAX as u64) as i32,
            nTrackPos: 0,
        };
        // SAFETY: `info` is fully set; the window is this view.
        unsafe {
            SetScrollInfo(self.hwnd, SB_VERT, &info, true);
        }
    }

    /// Runs `change` on the state, then repaints and updates the scroll bar.
    pub fn update(&self, change: impl FnOnce(&mut LogState)) {
        change(&mut self.state.borrow_mut());
        self.sync_scrollbar();
        invalidate(self.hwnd);
    }

    pub fn state<T>(&self, read: impl FnOnce(&LogState) -> T) -> T {
        read(&self.state.borrow())
    }

    /// Scrolls sideways by `columns` characters.
    pub fn shift_by(&self, columns: i32) {
        let limit = MAX_LINE as i32 * self.char_width.get();
        let shift = (self.shift.get() + columns * self.char_width.get()).clamp(0, limit);
        if self.shift.replace(shift) != shift {
            invalidate(self.hwnd);
        }
    }

    /// The row of the view at height `y`, which may be above or below it.
    pub fn row_at(&self, y: i32) -> i64 {
        i64::from(y.div_euclid(self.line_height.get().max(1)))
    }

    /// Copies the selected lines, without escape sequences, one per line.
    pub fn copy_selection(&self, owner: HWND) -> bool {
        let Some((first, last)) = self.state.borrow().selection() else {
            return false;
        };
        let Some(source) = self.source.borrow().clone() else {
            return false;
        };
        let count = usize::try_from(last - first + 1).unwrap_or(usize::MAX);
        let mut text: Vec<u16> = Vec::new();
        let mut line_units: Vec<u16> = Vec::new();
        let mut spans = Vec::new();
        source.visit(first, count, &mut |_, line| {
            line_units.clear();
            line_units.extend(line.encode_utf16().filter(|&unit| unit != u16::from(b'\r')));
            ansi::strip_into(&line_units, &mut spans, &mut text);
            text.extend_from_slice(&[u16::from(b'\r'), u16::from(b'\n')]);
        });
        copy_to_clipboard(owner, &text)
    }

    pub fn paint(&self, hdc: HDC, theme: &Theme, fonts: &Fonts, dpi: Dpi) {
        let area = client_rect(self.hwnd);
        let line_height = self.line_height.get().max(1);
        let rows = ((area.bottom + line_height - 1) / line_height).max(0) as usize;
        let state = self.state.borrow();
        let mut frame = self.frame.borrow_mut();
        let frame = &mut *frame;
        frame.text.clear();
        frame.lines.clear();
        if let Some(source) = self.source.borrow().as_ref() {
            let text = &mut frame.text;
            let lines = &mut frame.lines;
            source.visit(state.top(), rows, &mut |number, line| {
                sanitize_into(line, text);
                lines.push((number, text.len()));
            });
        }
        frame.ensure(hdc, area.right, area.bottom);
        let memory = frame.dc;
        let left = dpi.px(6) - self.shift.get();
        // SAFETY: `memory` is the frame's DC, `hdc` the paint DC; the font
        // is selected back out and the text alignment restored.
        unsafe {
            FillRect(memory, &area, theme.surface_brush);
            let old_font = SelectObject(memory, fonts.mono.into());
            SetBkMode(memory, TRANSPARENT);
            let dc_brush = HBRUSH(GetStockObject(DC_BRUSH).0);
            if frame.lines.is_empty() {
                SetTextColor(memory, theme.muted);
                let mut empty: Vec<u16> = "No log lines yet".encode_utf16().collect();
                let mut rect = area;
                DrawTextW(
                    memory,
                    &mut empty,
                    &mut rect,
                    DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                );
            }
            SetTextAlign(memory, TA_UPDATECP);
            let mut start = 0;
            for (row, &(number, end)) in frame.lines.iter().enumerate() {
                let y = row as i32 * line_height;
                if state.is_selected(number) {
                    SetDCBrushColor(memory, theme.selection);
                    let band = RECT {
                        left: 0,
                        top: y,
                        right: area.right,
                        bottom: y + line_height,
                    };
                    FillRect(memory, &band, dc_brush);
                }
                let line = &frame.text[start..end];
                start = end;
                ansi::parse(line, &mut frame.spans);
                let _ = MoveToEx(memory, left, y, None);
                for span in &frame.spans {
                    SetTextColor(memory, theme.ansi_color(span.style));
                    let _ = TextOutW(memory, 0, 0, &line[span.start..span.end]);
                }
            }
            SetTextAlign(memory, TA_NOUPDATECP);
            if self.focused.get() {
                SetDCBrushColor(memory, theme.accent);
                FrameRect(memory, &area, dc_brush);
            }
            SelectObject(memory, old_font);
            let _ = BitBlt(
                hdc,
                0,
                0,
                area.right,
                area.bottom,
                Some(memory),
                0,
                0,
                SRCCOPY,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean(line: &str) -> String {
        let mut out = Vec::new();
        sanitize_into(line, &mut out);
        String::from_utf16_lossy(&out)
    }

    #[test]
    fn tabs_expand_and_control_characters_go_but_escapes_stay() {
        assert_eq!(clean("a\tb"), "a    b");
        assert_eq!(clean("a\r\u{7}b\u{7f}"), "ab");
        assert_eq!(clean("\x1b[31m한글"), "\x1b[31m한글");
    }

    #[test]
    fn long_lines_are_capped_and_appended_after_what_is_there() {
        let mut out: Vec<u16> = vec![1, 2];
        sanitize_into(&"x".repeat(MAX_LINE * 2), &mut out);
        assert_eq!(out.len(), 2 + MAX_LINE);
    }
}
