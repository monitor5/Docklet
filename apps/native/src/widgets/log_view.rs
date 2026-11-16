//! The log view: a custom-painted window over a [`LogSource`].
//!
//! It keeps the page it last drew as a bitmap. A page that differs from it
//! by a few lines, as one that follows a log does, is that bitmap moved,
//! with only the new lines drawn ([`reuse`]); a page that does not differ
//! is not drawn at all. Copying text out of the source, splitting it into
//! colour runs and drawing it allocates nothing once the buffers have
//! grown. The source's lock is held only while copying.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{COLORREF, HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DC_BRUSH, DT_CENTER, DT_SINGLELINE,
    DT_VCENTER, DeleteDC, DeleteObject, DrawTextW, FillRect, FrameRect, GetStockObject, HBITMAP,
    HBRUSH, HDC, HFONT, HGDIOBJ, IntersectClipRect, MoveToEx, OPAQUE, RestoreDC, SRCCOPY, SaveDC,
    SelectObject, SetBkColor, SetBkMode, SetDCBrushColor, SetTextAlign, SetTextColor, TA_BASELINE,
    TA_NOUPDATECP, TA_UPDATECP, TRANSPARENT, TextOutW,
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

/// Drawing a frame may take one part in this many of the time: 2.5%.
const DRAWING_SHARE: u32 = 40;

/// How long after a frame that took `cost` to draw the next one may be
/// drawn, so that drawing stays within its share of a processor whatever a
/// page costs: a few lines that scroll in cost next to nothing and are
/// shown at once, a log that replaces the whole page with every frame is
/// shown less often, and less often still in a large window.
pub fn pause_after(cost: Duration) -> Duration {
    cost * DRAWING_SHARE
}

/// What the scroll bar is told: the last position, the page and the
/// position, counted from the first line held. While a full buffer drops a
/// line for each one it takes in, these stay the same though every line
/// number moves, and the bar need not be touched.
pub fn scroll_info(first: u64, end: u64, top: u64, page: u64) -> (i32, u32, i32) {
    let clamp = |value: u64| value.min(i32::MAX as u64) as i32;
    (
        clamp((end - first).saturating_sub(1)),
        page.min(u32::MAX as u64) as u32,
        clamp(top.saturating_sub(first)),
    )
}

/// Whether `unit` is Hangul that is drawn as it stands: a syllable or a
/// compatibility jamo. Conjoining jamo need shaping and are left to Windows.
pub fn is_hangul(unit: u16) -> bool {
    (0xac00..=0xd7a3).contains(&unit) || (0x3131..=0x318e).contains(&unit)
}

/// Splits `run` where Hangul begins and ends: each piece as its range and
/// whether it is Hangul. The log font has no Hangul; drawn in one piece,
/// Windows looks for a font that has it with every call, which costs
/// several times the drawing itself.
pub fn segments(run: &[u16]) -> impl Iterator<Item = (Range<usize>, bool)> {
    let mut start = 0;
    std::iter::from_fn(move || {
        let first = *run.get(start)?;
        let hangul = is_hangul(first);
        let length = run[start..]
            .iter()
            .take_while(|&&unit| is_hangul(unit) == hangul)
            .count();
        let segment = start..start + length;
        start += length;
        Some((segment, hangul))
    })
}

/// What a frame is drawn with.
pub(crate) struct Look<'a> {
    pub theme: &'a Theme,
    pub font: HFONT,
    /// The font for Hangul, if the system has it (see `Fonts::hangul`).
    pub hangul: Option<HFONT>,
    pub line_height: i32,
    /// From the top of a line down to the baseline of `font`.
    pub ascent: i32,
    /// Where a line starts: the margin, less what is scrolled sideways.
    pub left: i32,
}

/// Which rows of a page the bitmap already shows right, once its content
/// is moved by `shift` rows (down when positive). The rest is drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reuse {
    pub shift: i64,
    pub kept: Range<usize>,
}

impl Reuse {
    const NONE: Self = Self {
        shift: 0,
        kept: 0..0,
    };
}

/// What of a page drawn from line `was_top`, with `was_count` rows that
/// held a line, still serves a page from line `top`. `whole` rows fit the
/// view completely; `rows` counts the one cut off at the bottom too.
///
/// A line never changes once it is written, and it keeps its number. So a
/// page that follows a log by a line or two is the old page moved up, with
/// those lines drawn under it: the cost of a frame is that of the lines
/// that are new, not of the page.
pub fn reuse(was_top: u64, was_count: usize, top: u64, whole: usize, rows: usize) -> Reuse {
    // A row cut off at the bottom is right where it is, and nowhere else.
    let complete = was_count.min(whole);
    if top == was_top {
        return Reuse {
            shift: 0,
            kept: 0..was_count.min(rows),
        };
    }
    if top > was_top {
        return match usize::try_from(top - was_top) {
            Ok(up) if up < complete => Reuse {
                shift: -(up as i64),
                kept: 0..complete - up,
            },
            _ => Reuse::NONE,
        };
    }
    match usize::try_from(was_top - top) {
        Ok(down) if down < rows && complete > 0 => Reuse {
            shift: down as i64,
            kept: down..(down + complete).min(rows),
        },
        _ => Reuse::NONE,
    }
}

/// How a page looks apart from which lines it shows. When this differs,
/// nothing of the bitmap is right any more.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Appearance {
    dark: bool,
    font: isize,
    line_height: i32,
    left: i32,
    selection: Option<(u64, u64)>,
}

/// What the bitmap shows: `count` lines from line `top` on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Drawn {
    top: u64,
    count: usize,
    appearance: Appearance,
}

/// One page of the log: its text, and the bitmap it is drawn into.
#[derive(Default)]
pub(crate) struct Frame {
    /// The lines being drawn, back to back.
    text: Vec<u16>,
    /// Number and end offset in `text` of each of them.
    lines: Vec<(u64, usize)>,
    spans: Vec<Span>,
    /// The page as it was last drawn, kept between frames and dropped when
    /// the view hides.
    dc: HDC,
    bitmap: HBITMAP,
    old_bitmap: HGDIOBJ,
    size: (i32, i32),
    /// What the bitmap shows, while it shows anything of use.
    drawn: Option<Drawn>,
}

impl Frame {
    fn release(&mut self) {
        self.drawn = None;
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

    /// Forgets what the bitmap shows: the next page is drawn whole.
    pub(crate) fn forget(&mut self) {
        self.drawn = None;
    }

    /// Copies up to `rows` lines from `top` on out of `source`, or none.
    fn load(&mut self, source: Option<&dyn LogSource>, top: u64, rows: usize) {
        self.text.clear();
        self.lines.clear();
        if let Some(source) = source {
            let text = &mut self.text;
            let lines = &mut self.lines;
            source.visit(top, rows, &mut |number, line| {
                sanitize_into(line, text);
                lines.push((number, text.len()));
            });
        }
    }

    /// Makes the bitmap show the lines `top..end` of `source` that fit
    /// `area`, drawing only what it does not show already. `hdc` is a DC
    /// of the window, to make the bitmap for.
    pub(crate) fn update(
        &mut self,
        hdc: HDC,
        area: RECT,
        source: Option<&dyn LogSource>,
        (top, end): (u64, u64),
        selection: Option<(u64, u64)>,
        look: &Look<'_>,
    ) {
        self.ensure(hdc, area.right, area.bottom);
        let line_height = look.line_height.max(1);
        let rows = ((area.bottom + line_height - 1) / line_height).max(0) as usize;
        let whole = (area.bottom / line_height).max(0) as usize;
        let count = end.saturating_sub(top).min(rows as u64) as usize;
        let appearance = Appearance {
            dark: look.theme.dark,
            font: look.font.0 as isize,
            line_height,
            left: look.left,
            selection,
        };
        let wanted = Drawn {
            top,
            count,
            appearance,
        };
        if self.drawn == Some(wanted) {
            return;
        }
        let reuse = match self.drawn {
            Some(was) if was.appearance == appearance => {
                reuse(was.top, was.count, top, whole, rows)
            }
            _ => Reuse::NONE,
        };
        // Never more rows than there are lines now.
        let kept = reuse.kept.start.min(count)..reuse.kept.end.min(count);
        let memory = self.dc;
        if reuse.shift != 0 && !kept.is_empty() {
            let by = reuse.shift as i32 * line_height;
            // SAFETY: copies within the frame's own bitmap; GDI handles a
            // source and a destination that overlap.
            unsafe {
                let _ = BitBlt(
                    memory,
                    0,
                    by.max(0),
                    area.right,
                    area.bottom - by.abs(),
                    Some(memory),
                    0,
                    (-by).max(0),
                    SRCCOPY,
                );
            }
        }
        self.draw_rows(0..kept.start, area, source, (top, count), selection, look);
        self.draw_rows(kept.end..rows, area, source, (top, count), selection, look);
        if count == 0 {
            // SAFETY: `memory` is the frame's DC; the font is selected back
            // out.
            unsafe {
                let old_font = SelectObject(memory, look.font.into());
                SetBkMode(memory, TRANSPARENT);
                SetTextColor(memory, look.theme.muted);
                let mut empty: Vec<u16> = "No log lines yet".encode_utf16().collect();
                let mut rect = area;
                DrawTextW(
                    memory,
                    &mut empty,
                    &mut rect,
                    DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                );
                SelectObject(memory, old_font);
            }
        }
        self.drawn = Some(wanted);
    }

    /// Draws the rows `rows` of a page that shows `count` lines from line
    /// `top` on: the background of each, and its line if it has one.
    fn draw_rows(
        &mut self,
        rows: Range<usize>,
        area: RECT,
        source: Option<&dyn LogSource>,
        (top, count): (u64, usize),
        selection: Option<(u64, u64)>,
        look: &Look<'_>,
    ) {
        if rows.is_empty() {
            return;
        }
        let with_lines = rows.start..rows.end.min(count).max(rows.start);
        self.load(source, top + rows.start as u64, with_lines.len());
        let memory = self.dc;
        let theme = look.theme;
        let line_height = look.line_height.max(1);
        let selected =
            |number: u64| selection.is_some_and(|(first, last)| (first..=last).contains(&number));
        // SAFETY: `memory` is the frame's DC; the font is selected back out
        // and the text alignment restored.
        unsafe {
            let band = RECT {
                left: 0,
                top: rows.start as i32 * line_height,
                right: area.right,
                bottom: (rows.end as i32 * line_height).min(area.bottom),
            };
            FillRect(memory, &band, theme.surface_brush);
            let old_font = SelectObject(memory, look.font.into());
            let dc_brush = HBRUSH(GetStockObject(DC_BRUSH).0);
            // On the baseline, so text in two fonts lines up. Opaque, since
            // blending each glyph into what is there costs half as much
            // again; a line of the log font is exactly as tall as its row.
            SetTextAlign(memory, TA_UPDATECP | TA_BASELINE);
            SetBkMode(memory, OPAQUE);
            let mut start = 0;
            for (offset, &(number, end)) in self.lines.iter().enumerate() {
                let y = (rows.start + offset) as i32 * line_height;
                let background = if selected(number) {
                    SetDCBrushColor(memory, theme.selection);
                    let band = RECT {
                        left: 0,
                        top: y,
                        right: area.right,
                        bottom: y + line_height,
                    };
                    FillRect(memory, &band, dc_brush);
                    theme.selection
                } else {
                    theme.surface
                };
                SetBkColor(memory, background);
                let line = &self.text[start..end];
                start = end;
                ansi::parse(line, &mut self.spans);
                let _ = MoveToEx(memory, look.left, y + look.ascent, None);
                for span in &self.spans {
                    SetTextColor(memory, theme.ansi_color(span.style));
                    let run = &line[span.start..span.end];
                    let Some(hangul) = look.hangul else {
                        let _ = TextOutW(memory, 0, 0, run);
                        continue;
                    };
                    for (segment, is_hangul) in segments(run) {
                        if !is_hangul {
                            let _ = TextOutW(memory, 0, 0, &run[segment]);
                            continue;
                        }
                        // Its cell is taller than the row: opaque, it
                        // would cut into the lines above and below.
                        SelectObject(memory, hangul.into());
                        SetBkMode(memory, TRANSPARENT);
                        let _ = TextOutW(memory, 0, 0, &run[segment]);
                        SetBkMode(memory, OPAQUE);
                        SelectObject(memory, look.font.into());
                    }
                }
            }
            SetTextAlign(memory, TA_NOUPDATECP);
            SetBkMode(memory, TRANSPARENT);
            SelectObject(memory, old_font);
        }
    }

    /// Copies the part `rect` of the page to `hdc`. A view that has the
    /// focus gets a frame around it, which is no part of the bitmap: the
    /// bitmap's content moves when the log does.
    pub(crate) fn blit(&self, hdc: HDC, rect: RECT, area: RECT, focus: Option<COLORREF>) {
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        // SAFETY: `hdc` is the caller's DC, the source the frame's own; the
        // DC's state is saved and restored around the clipping.
        unsafe {
            let Some(color) = focus else {
                let _ = BitBlt(
                    hdc,
                    rect.left,
                    rect.top,
                    width,
                    height,
                    Some(self.dc),
                    rect.left,
                    rect.top,
                    SRCCOPY,
                );
                return;
            };
            // The frame's pixels are painted once, by the frame: copying
            // the page over them first would make it flicker.
            let saved = SaveDC(hdc);
            IntersectClipRect(hdc, 1, 1, area.right - 1, area.bottom - 1);
            let _ = BitBlt(
                hdc,
                rect.left,
                rect.top,
                width,
                height,
                Some(self.dc),
                rect.left,
                rect.top,
                SRCCOPY,
            );
            let _ = RestoreDC(hdc, saved);
            SetDCBrushColor(hdc, color);
            FrameRect(hdc, &area, HBRUSH(GetStockObject(DC_BRUSH).0));
        }
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        self.release();
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
    /// What the scroll bar was last told, as [`scroll_info`] gives it.
    scrollbar: Cell<Option<(i32, u32, i32)>>,
    focused: Cell<bool>,
    frame: RefCell<Frame>,
    /// Not before this is the next frame taken from the source (see
    /// [`pause_after`]).
    rest_until: Cell<Option<Instant>>,
    /// The source changed since the status line was last brought up to
    /// date (see [`Self::take_news`]).
    news: Cell<bool>,
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
            scrollbar: Cell::new(None),
            focused: Cell::new(false),
            frame: RefCell::new(Frame::default()),
            rest_until: Cell::new(None),
            news: Cell::new(false),
        }
    }

    /// Whether lines arrived, or the log ended, since this was last asked:
    /// then the count in the status line and the scroll bar are out of
    /// date, also for a paused view whose page did not change. On a quiet
    /// log this stays false, and nothing is recomputed.
    pub fn take_news(&self) -> bool {
        self.news.replace(false)
    }

    /// A frame took `cost` to paint: the next one waits its turn.
    pub fn painted(&self, cost: Duration) {
        self.rest_until
            .set(Some(Instant::now() + pause_after(cost)));
    }

    /// Whether the last frame was cheap enough, or long enough ago, for
    /// the next one to be due.
    pub fn is_rested(&self) -> bool {
        self.rest_until
            .get()
            .is_none_or(|until| Instant::now() >= until)
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
        // Another log numbers its lines anew.
        self.frame.borrow_mut().forget();
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
        *self.frame.borrow_mut() = Frame::default();
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

    /// Reads the source's bounds if it changed. Returns whether what the
    /// view shows changed with them: lines that pile up below a view that
    /// does not follow, or fall out above it, change nothing on screen.
    pub fn poll(&self) -> bool {
        let Some(source) = self.source.borrow().clone() else {
            return false;
        };
        let generation = source.generation();
        if self.seen.replace(generation) == generation {
            return false;
        }
        self.news.set(true);
        let (first, end) = source.bounds();
        let mut state = self.state.borrow_mut();
        let shown = |state: &LogState| (state.top(), state.end().min(state.top() + state.page()));
        let before = shown(&state);
        let selection = state.selection();
        state.set_bounds(first, end);
        before != shown(&state) || selection != state.selection()
    }

    /// Tells the scroll bar where the view is, if that changed.
    pub fn sync_scrollbar(&self) {
        let wanted = {
            let state = self.state.borrow();
            scroll_info(state.first(), state.end(), state.top(), state.page())
        };
        if self.scrollbar.replace(Some(wanted)) == Some(wanted) {
            return;
        }
        let (last, page, position) = wanted;
        let info = SCROLLINFO {
            cbSize: size_of::<SCROLLINFO>() as u32,
            fMask: SIF_RANGE | SIF_PAGE | SIF_POS | SIF_DISABLENOSCROLL,
            nMin: 0,
            nMax: last,
            nPage: page,
            nPos: position,
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

    /// Paints the part `rect` of the view: brings the page up to date,
    /// which costs nothing when it is, and copies that part of it.
    pub fn paint(&self, hdc: HDC, rect: RECT, theme: &Theme, fonts: &Fonts, dpi: Dpi) {
        let area = client_rect(self.hwnd);
        let state = self.state.borrow();
        let mut frame = self.frame.borrow_mut();
        let look = Look {
            theme,
            font: fonts.mono,
            hangul: fonts.hangul,
            line_height: self.line_height.get().max(1),
            ascent: fonts.mono_ascent,
            left: dpi.px(6) - self.shift.get(),
        };
        frame.update(
            hdc,
            area,
            self.source.borrow().as_deref(),
            (state.top(), state.end()),
            state.selection(),
            &look,
        );
        let focus = self.focused.get().then_some(theme.accent);
        frame.blit(hdc, rect, area, focus);
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

    #[test]
    fn a_full_buffer_that_keeps_scrolling_tells_the_scroll_bar_nothing_new() {
        // 50,000 lines held, following, 40 to a page.
        let following = scroll_info(1_000, 51_000, 50_960, 40);
        assert_eq!(following, (49_999, 40, 49_960));
        // 500 lines later every number has moved, and nothing else.
        assert_eq!(scroll_info(1_500, 51_500, 51_460, 40), following);
        // A buffer that still grows does move the bar.
        assert_ne!(
            scroll_info(0, 600, 560, 40),
            scroll_info(0, 1_100, 1_060, 40)
        );
        assert_eq!(scroll_info(0, 0, 0, 1), (0, 1, 0));
    }

    fn kept(shift: i64, kept: Range<usize>) -> Reuse {
        Reuse { shift, kept }
    }

    #[test]
    fn a_page_that_follows_a_log_is_the_old_page_moved_up() {
        // 40 whole rows and one cut off; all 41 held a line.
        // Two lines arrived: the page moves up two rows, and the rows from
        // 38 on are drawn (two new lines and the cut one).
        assert_eq!(reuse(100, 41, 102, 40, 41), kept(-2, 0..38));
        // Nothing arrived: nothing to draw.
        assert_eq!(reuse(100, 41, 100, 40, 41), kept(0, 0..41));
        // A page or more arrived, as at 10,000 lines a second: all of it
        // is drawn.
        assert_eq!(reuse(100, 41, 140, 40, 41), Reuse::NONE);
        assert_eq!(reuse(100, 41, 9_000, 40, 41), Reuse::NONE);
        // One row short of a page still saves that row.
        assert_eq!(reuse(100, 41, 139, 40, 41), kept(-39, 0..1));
    }

    #[test]
    fn a_log_shorter_than_the_view_grows_at_its_end() {
        // 5 lines of a log that starts at line 0: three more arrive, and
        // rows 0 to 4 stay as they are.
        assert_eq!(reuse(0, 5, 0, 40, 41), kept(0, 0..5));
        // Nothing was drawn yet: nothing to keep.
        assert_eq!(reuse(0, 0, 0, 40, 41), kept(0, 0..0));
    }

    #[test]
    fn scrolling_back_moves_the_page_down() {
        // Three lines up: rows 3 to 40 hold what rows 0 to 37 held, cut
        // off at the bottom as the last row always is.
        assert_eq!(reuse(100, 41, 97, 40, 41), kept(3, 3..41));
        // A view that fits its rows exactly loses the last three.
        assert_eq!(reuse(100, 40, 97, 40, 40), kept(3, 3..40));
        // A page or more up: all of it is drawn.
        assert_eq!(reuse(100, 41, 59, 40, 41), Reuse::NONE);
        assert_eq!(reuse(100, 41, 0, 40, 41), Reuse::NONE);
        // A view too small for a whole row has nothing worth moving.
        assert_eq!(reuse(100, 1, 99, 0, 1), Reuse::NONE);
        assert_eq!(reuse(100, 1, 101, 0, 1), Reuse::NONE);
    }

    #[test]
    fn drawing_keeps_to_its_share_whatever_a_frame_costs() {
        // Two lines scrolled in: the next 80 ms tick is not held up.
        assert!(pause_after(Duration::from_micros(300)) < Duration::from_millis(80));
        // A whole page of a large window: about four frames a second.
        assert_eq!(
            pause_after(Duration::from_millis(6)),
            Duration::from_millis(240)
        );
        assert_eq!(pause_after(Duration::ZERO), Duration::ZERO);
    }

    fn pieces(text: &str) -> Vec<(String, bool)> {
        let run: Vec<u16> = text.encode_utf16().collect();
        segments(&run)
            .map(|(segment, hangul)| (String::from_utf16_lossy(&run[segment]), hangul))
            .collect()
    }

    #[test]
    fn a_run_is_split_where_hangul_begins_and_ends() {
        assert_eq!(
            pieces(" 12ms 한글 로그"),
            [
                (" 12ms ".to_string(), false),
                ("한글".to_string(), true),
                (" ".to_string(), false),
                ("로그".to_string(), true),
            ]
        );
        assert_eq!(pieces("plain"), [("plain".to_string(), false)]);
        assert_eq!(pieces("오류"), [("오류".to_string(), true)]);
        assert!(pieces("").is_empty());
    }

    #[test]
    fn only_hangul_that_needs_no_shaping_takes_the_short_way() {
        // Compatibility jamo stand alone, like syllables.
        assert_eq!(
            pieces("ㅋㅋ ok"),
            [("ㅋㅋ".to_string(), true), (" ok".to_string(), false)]
        );
        // Kana, Han and conjoining jamo are left to Windows.
        assert_eq!(pieces("かな中文\u{1100}\u{1161}").len(), 1);
        assert!(!is_hangul(u16::from(b'a')) && !is_hangul(0xd7a4) && is_hangul(0xd7a3));
    }
}
