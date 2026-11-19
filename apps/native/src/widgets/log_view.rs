//! The log view: a custom-painted window over a [`LogSource`].
//!
//! It keeps the page it last drew as a bitmap. A page that differs from it
//! by a few lines, as one that follows a log does, is that bitmap moved,
//! with only the new lines drawn ([`reuse`]); a page that does not differ
//! is not drawn at all. The bitmap knows how far right each of its rows is
//! drawn, so what is moved, cleared and copied to the window is as wide as
//! the text and not as wide as the view ([`changed_area`]): a few short
//! lines that scroll in cost the same in a small window and a maximised
//! one. Copying text out of the source, splitting it into colour runs and
//! drawing it allocates nothing once the buffers have grown. The source's
//! lock is held only while copying.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DC_BRUSH, DT_CENTER, DT_SINGLELINE,
    DT_VCENTER, DeleteDC, DeleteObject, DrawTextW, FillRect, FrameRect, GetCurrentPositionEx,
    GetDC, GetStockObject, HBITMAP, HBRUSH, HDC, HFONT, HGDIOBJ, IntersectClipRect, InvalidateRect,
    MoveToEx, OPAQUE, ReleaseDC, RestoreDC, SRCCOPY, SaveDC, SelectObject, SetBkColor, SetBkMode,
    SetDCBrushColor, SetTextAlign, SetTextColor, TA_BASELINE, TA_NOUPDATECP, TA_UPDATECP,
    TRANSPARENT, TextOutW,
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

/// The least time between two frames: a log that keeps running is drawn
/// about three times a second. A frame costs a third of a millisecond
/// before its first line is drawn, and the first line of a frame costs
/// three times what the next ones do (measured in the window: everything
/// it touches has gone cold since the last frame), so the same lines in
/// fewer frames cost half as much. A line that arrives after a pause is
/// still shown with the next tick of the timer.
const LEAST_PAUSE: Duration = Duration::from_millis(250);

/// How long after a frame that took `cost` to draw the next one may be
/// drawn, so that drawing stays within its share of a processor whatever a
/// page costs: a log that replaces the whole page with every frame is
/// shown less often, and less often still in a large window.
pub fn pause_after(cost: Duration) -> Duration {
    (cost * DRAWING_SHARE).max(LEAST_PAUSE)
}

/// How far right a page is drawn, given how far each of its rows is.
pub fn widest(extents: &[i32]) -> i32 {
    extents.iter().copied().max().unwrap_or(0).max(0)
}

/// How far right anything is drawn in the rows `rows`: by themselves, or by
/// the row above and the row below, whose taller glyphs (Hangul in the
/// font that has it) may reach a pixel or two into them.
pub fn reach(extents: &[i32], rows: Range<usize>) -> i32 {
    let end = (rows.end + 1).min(extents.len());
    let start = rows.start.saturating_sub(1).min(end);
    widest(&extents[start..end])
}

/// What a frame changed on the page: some rows, as far right as `width`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub rows: Range<usize>,
    pub width: i32,
}

impl Change {
    pub const NONE: Self = Self {
        rows: 0..0,
        width: 0,
    };

    /// A change that covers this one and `other`.
    pub fn join(self, other: Self) -> Self {
        if self.rows.is_empty() {
            return other;
        }
        if other.rows.is_empty() {
            return self;
        }
        Self {
            rows: self.rows.start.min(other.rows.start)..self.rows.end.max(other.rows.end),
            width: self.width.max(other.width),
        }
    }
}

/// The pixels of a view `right` by `bottom` that `change` touches, as
/// `(left, top, right, bottom)`, or `None` if it touches none. Only these
/// are copied to the window: the rest of it shows what it should already.
pub fn changed_area(
    change: &Change,
    line_height: i32,
    right: i32,
    bottom: i32,
) -> Option<(i32, i32, i32, i32)> {
    let line_height = line_height.max(1);
    let row = |row: usize| i32::try_from(row).unwrap_or(i32::MAX);
    let top = row(change.rows.start).saturating_mul(line_height);
    let end = row(change.rows.end).saturating_mul(line_height).min(bottom);
    let width = change.width.min(right);
    (!change.rows.is_empty() && width > 0 && top < end).then_some((0, top, width, end))
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
/// held a line, still serves a page from line `top`. `rows` is how many
/// rows the page has, the one cut off at the bottom of the view included:
/// the bitmap holds that one whole, so it moves like any other.
///
/// A line never changes once it is written, and it keeps its number. So a
/// page that follows a log by a line or two is the old page moved up, with
/// those lines drawn under it: the cost of a frame is that of the lines
/// that are new, not of the page.
pub fn reuse(was_top: u64, was_count: usize, top: u64, rows: usize) -> Reuse {
    let was_count = was_count.min(rows);
    if top == was_top {
        return Reuse {
            shift: 0,
            kept: 0..was_count,
        };
    }
    if top > was_top {
        return match usize::try_from(top - was_top) {
            Ok(up) if up < was_count => Reuse {
                shift: -(up as i64),
                kept: 0..was_count - up,
            },
            _ => Reuse::NONE,
        };
    }
    match usize::try_from(was_top - top) {
        Ok(down) if down < rows && was_count > 0 => Reuse {
            shift: down as i64,
            kept: down..(down + was_count).min(rows),
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
    /// Per row of the page, how far right it is drawn: right of that the
    /// bitmap is plain background. A selected row is drawn all the way.
    extents: Vec<i32>,
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
    /// of the window, to make the bitmap for. Returns the part of the view
    /// that differs from what the bitmap showed before, if any does.
    pub(crate) fn update(
        &mut self,
        hdc: HDC,
        area: RECT,
        source: Option<&dyn LogSource>,
        (top, end): (u64, u64),
        selection: Option<(u64, u64)>,
        look: &Look<'_>,
    ) -> Option<RECT> {
        let line_height = look.line_height.max(1);
        let rows = ((area.bottom + line_height - 1) / line_height).max(0) as usize;
        // Whole rows, the one the view cuts off included: it moves with
        // the others, and is not drawn again with every frame.
        self.ensure(hdc, area.right, rows as i32 * line_height);
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
            return None;
        }
        let reuse = match self.drawn {
            Some(was) if was.appearance == appearance => reuse(was.top, was.count, top, rows),
            _ => {
                // Nothing of the bitmap is right: every row is cleared all
                // the way across before it is drawn.
                self.extents.clear();
                self.extents.resize(rows, area.right);
                Reuse::NONE
            }
        };
        // Never more rows than there are lines now.
        let kept = reuse.kept.start.min(count)..reuse.kept.end.min(count);
        let memory = self.dc;
        let before = widest(&self.extents).min(area.right);
        let moved = reuse.shift != 0 && !kept.is_empty();
        if moved {
            let shift = reuse.shift as isize;
            let from = kept.start.wrapping_add_signed(-shift)..kept.end.wrapping_add_signed(-shift);
            // As wide as the widest row: right of it both the rows that
            // move and the rows they move onto are background.
            if before > 0 {
                // SAFETY: copies within the frame's own bitmap; GDI handles
                // a source and a destination that overlap.
                unsafe {
                    let _ = BitBlt(
                        memory,
                        0,
                        kept.start as i32 * line_height,
                        before,
                        kept.len() as i32 * line_height,
                        Some(memory),
                        0,
                        from.start as i32 * line_height,
                        SRCCOPY,
                    );
                }
            }
            // The rows that were not moved onto still show what they did.
            self.extents.copy_within(from, kept.start);
        }
        let above = self.draw_rows(0..kept.start, area, source, (top, count), selection, look);
        let below = self.draw_rows(kept.end..rows, area, source, (top, count), selection, look);
        self.drawn = Some(wanted);
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
            // Somewhere across the rows: all of them go before the first
            // line is drawn.
            self.extents.fill(area.right);
            return Some(area);
        }
        let change = if moved {
            // Every row shows another line than it did.
            Change {
                rows: 0..rows,
                width: before.max(widest(&self.extents)),
            }
        } else {
            above.join(below)
        };
        changed_area(&change, line_height, area.right, area.bottom).map(
            |(left, top, right, bottom)| RECT {
                left,
                top,
                right,
                bottom,
            },
        )
    }

    /// Draws the rows `rows` of a page that shows `count` lines from line
    /// `top` on: clears what each shows now, and draws its line if it has
    /// one. Returns what that changed.
    fn draw_rows(
        &mut self,
        rows: Range<usize>,
        area: RECT,
        source: Option<&dyn LogSource>,
        (top, count): (u64, usize),
        selection: Option<(u64, u64)>,
        look: &Look<'_>,
    ) -> Change {
        if rows.is_empty() {
            return Change::NONE;
        }
        let with_lines = rows.start..rows.end.min(count).max(rows.start);
        self.load(source, top + rows.start as u64, with_lines.len());
        let memory = self.dc;
        let theme = look.theme;
        let line_height = look.line_height.max(1);
        let selected =
            |number: u64| selection.is_some_and(|(first, last)| (first..=last).contains(&number));
        // As far as anything reaches into these rows; right of that they
        // are background already, and clearing it is most of what a page
        // of short lines would cost.
        let stale = reach(&self.extents, rows.clone()).min(area.right);
        self.extents[rows.clone()].fill(0);
        // SAFETY: `memory` is the frame's DC; the font is selected back out
        // and the text alignment restored.
        unsafe {
            if stale > 0 {
                let band = RECT {
                    left: 0,
                    top: rows.start as i32 * line_height,
                    right: stale,
                    bottom: rows.end as i32 * line_height,
                };
                FillRect(memory, &band, theme.surface_brush);
            }
            let old_font = SelectObject(memory, look.font.into());
            let dc_brush = HBRUSH(GetStockObject(DC_BRUSH).0);
            // On the baseline, so text in two fonts lines up. Opaque, since
            // blending each glyph into what is there costs half as much
            // again; a line of the log font is exactly as tall as its row.
            SetTextAlign(memory, TA_UPDATECP | TA_BASELINE);
            SetBkMode(memory, OPAQUE);
            let mut start = 0;
            for (offset, &(number, end)) in self.lines.iter().enumerate() {
                let row = rows.start + offset;
                let y = row as i32 * line_height;
                let is_selected = selected(number);
                let background = if is_selected {
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
                // Where the text ended is how far the row is drawn, and a
                // little further: the last glyph and its smoothing may
                // hang over the place the next one would start at.
                let mut end = POINT::default();
                let _ = GetCurrentPositionEx(memory, &mut end);
                if let Some(extent) = self.extents.get_mut(row) {
                    *extent = if is_selected {
                        area.right
                    } else {
                        (end.x + line_height).clamp(0, area.right)
                    };
                }
            }
            SetTextAlign(memory, TA_NOUPDATECP);
            SetBkMode(memory, TRANSPARENT);
            SelectObject(memory, old_font);
        }
        let width = stale.max(reach(&self.extents, rows.clone()).min(area.right));
        if width == 0 {
            return Change::NONE;
        }
        // A glyph taller than its row reaches into the row above or below.
        Change {
            rows: rows.start.saturating_sub(1)..(rows.end + 1).min(self.extents.len()),
            width,
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
    /// What drawing the frame that is about to be painted has cost so far.
    spent: Cell<Duration>,
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
            spent: Cell::new(Duration::ZERO),
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

    /// A frame took `cost` to paint, on top of what drawing it into the
    /// bitmap took: the next one waits its turn.
    pub fn painted(&self, cost: Duration) {
        let cost = self.spent.replace(Duration::ZERO) + cost;
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

    /// Makes the bitmap show the page as the state has it now, which costs
    /// nothing when it does. Returns the part of the view that changed.
    fn bring_up_to_date(
        &self,
        hdc: HDC,
        area: RECT,
        theme: &Theme,
        fonts: &Fonts,
        dpi: Dpi,
    ) -> Option<RECT> {
        let state = self.state.borrow();
        let look = Look {
            theme,
            font: fonts.mono,
            hangul: fonts.hangul,
            line_height: self.line_height.get().max(1),
            ascent: fonts.mono_ascent,
            left: dpi.px(6) - self.shift.get(),
        };
        self.frame.borrow_mut().update(
            hdc,
            area,
            self.source.borrow().as_deref(),
            (state.top(), state.end()),
            state.selection(),
            &look,
        )
    }

    /// Draws what the last [`Self::poll`] brought into the bitmap and asks
    /// for a repaint of the part of the view that changed, and of no more:
    /// a few short lines that scrolled in are a strip at the left of a
    /// wide window.
    pub fn refresh(&self, theme: &Theme, fonts: &Fonts, dpi: Dpi) {
        let started = Instant::now();
        let area = client_rect(self.hwnd);
        // SAFETY: a DC of this view, released again; only the bitmap that
        // is made for it is drawn into. Invalidating marks pixels and calls
        // nothing back.
        unsafe {
            let hdc = GetDC(Some(self.hwnd));
            let changed = self.bring_up_to_date(hdc, area, theme, fonts, dpi);
            ReleaseDC(Some(self.hwnd), hdc);
            if let Some(changed) = changed {
                let _ = InvalidateRect(Some(self.hwnd), Some(&changed), false);
            }
        }
        self.spent.set(self.spent.get() + started.elapsed());
    }

    /// Paints the part `rect` of the view: brings the page up to date,
    /// which costs nothing when it is, and copies that part of it.
    pub fn paint(&self, hdc: HDC, rect: RECT, theme: &Theme, fonts: &Fonts, dpi: Dpi) {
        let area = client_rect(self.hwnd);
        let late = self.bring_up_to_date(hdc, area, theme, fonts, dpi);
        let focus = self.focused.get().then_some(theme.accent);
        self.frame.borrow().blit(hdc, rect, area, focus);
        // The page changed without the view being told which part (the
        // state moved between a repaint and this one): what lies outside
        // this paint is painted by the next.
        let covered = |late: &RECT| {
            rect.left <= late.left
                && rect.top <= late.top
                && rect.right >= late.right
                && rect.bottom >= late.bottom
        };
        if let Some(late) = late.filter(|late| !covered(late)) {
            // SAFETY: marks pixels of this view; nothing is called back.
            unsafe {
                let _ = InvalidateRect(Some(self.hwnd), Some(&late), false);
            }
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
        // 41 rows, the last one cut off by the view; all held a line.
        // Two lines arrived: the page moves up two rows, the cut one with
        // the others, and only the two new lines are drawn.
        assert_eq!(reuse(100, 41, 102, 41), kept(-2, 0..39));
        // Nothing arrived: nothing to draw.
        assert_eq!(reuse(100, 41, 100, 41), kept(0, 0..41));
        // A page or more arrived, as at 10,000 lines a second: all of it
        // is drawn.
        assert_eq!(reuse(100, 41, 141, 41), Reuse::NONE);
        assert_eq!(reuse(100, 41, 9_000, 41), Reuse::NONE);
        // One row short of a page still saves that row.
        assert_eq!(reuse(100, 41, 140, 41), kept(-40, 0..1));
    }

    #[test]
    fn a_log_shorter_than_the_view_grows_at_its_end() {
        // 5 lines of a log that starts at line 0: three more arrive, and
        // rows 0 to 4 stay as they are.
        assert_eq!(reuse(0, 5, 0, 41), kept(0, 0..5));
        // Nothing was drawn yet: nothing to keep.
        assert_eq!(reuse(0, 0, 0, 41), kept(0, 0..0));
    }

    #[test]
    fn scrolling_back_moves_the_page_down() {
        // Three lines up: rows 3 to 40 hold what rows 0 to 37 held.
        assert_eq!(reuse(100, 41, 97, 41), kept(3, 3..41));
        // A page that was not full keeps all of its lines.
        assert_eq!(reuse(100, 10, 97, 41), kept(3, 3..13));
        // A page or more up: all of it is drawn.
        assert_eq!(reuse(100, 41, 59, 41), Reuse::NONE);
        assert_eq!(reuse(100, 41, 0, 41), Reuse::NONE);
        // A view of one row has nothing to move.
        assert_eq!(reuse(100, 1, 99, 1), Reuse::NONE);
        assert_eq!(reuse(100, 1, 101, 1), Reuse::NONE);
        // More lines than the page has rows now are not kept.
        assert_eq!(reuse(100, 41, 100, 30), kept(0, 0..30));
    }

    #[test]
    fn drawing_keeps_to_its_share_whatever_a_frame_costs() {
        // A few lines scrolled in: the next three 80 ms ticks wait, the
        // fourth draws. A log that trickles is drawn three times a second.
        let trickle = pause_after(Duration::from_micros(300));
        assert!(trickle > Duration::from_millis(240) && trickle <= Duration::from_millis(320));
        assert_eq!(pause_after(Duration::ZERO), trickle);
        // A whole page of a large window: about two frames a second.
        assert_eq!(
            pause_after(Duration::from_millis(12)),
            Duration::from_millis(480)
        );
        // Never more than a fortieth of the time, whatever a page costs.
        for millis in [1, 5, 20, 80] {
            let cost = Duration::from_millis(millis);
            assert!(pause_after(cost) >= cost * 40);
        }
    }

    #[test]
    fn a_page_is_as_wide_as_its_widest_row_and_a_row_reaches_into_its_neighbours() {
        let extents = [300, 0, 520, 80, 0, 0];
        assert_eq!(widest(&extents), 520);
        assert_eq!(widest(&[]), 0);
        // Row 4 is empty, and so are its neighbours but row 3.
        assert_eq!(reach(&extents, 4..5), 80);
        // Rows 0 and 1, and what row 2 may put into row 1.
        assert_eq!(reach(&extents, 0..2), 520);
        assert_eq!(reach(&extents, 5..6), 0);
        assert_eq!(reach(&extents, 0..6), 520);
        // Rows that are not there reach nowhere.
        assert_eq!(reach(&extents, 9..12), 0);
        assert_eq!(reach(&[], 0..0), 0);
    }

    #[test]
    fn only_what_a_frame_changed_is_copied_to_the_window() {
        let change = |rows: Range<usize>, width: i32| Change { rows, width };
        // A view 1300 by 770 with rows of 19 pixels: 41 rows, the last one
        // cut off. Lines of 500 pixels scrolled in: a strip at the left.
        assert_eq!(
            changed_area(&change(0..41, 500), 19, 1300, 770),
            Some((0, 0, 500, 770))
        );
        // Two lines added under a short log: their rows alone.
        assert_eq!(
            changed_area(&change(5..7, 420), 19, 1300, 770),
            Some((0, 95, 420, 133))
        );
        // A selected line is drawn all the way across, and never further.
        assert_eq!(
            changed_area(&change(3..4, 5000), 19, 1300, 770),
            Some((0, 57, 1300, 76))
        );
        // Nothing changed, or nothing of it is in view.
        assert_eq!(changed_area(&Change::NONE, 19, 1300, 770), None);
        assert_eq!(changed_area(&change(0..41, 0), 19, 1300, 770), None);
        assert_eq!(changed_area(&change(41..42, 500), 19, 1300, 770), None);
    }

    #[test]
    fn two_changes_join_into_one_that_covers_both() {
        let change = |rows: Range<usize>, width: i32| Change { rows, width };
        assert_eq!(
            change(0..2, 300).join(change(38..41, 520)),
            change(0..41, 520)
        );
        assert_eq!(Change::NONE.join(change(3..4, 10)), change(3..4, 10));
        assert_eq!(change(3..4, 10).join(Change::NONE), change(3..4, 10));
        assert_eq!(Change::NONE.join(Change::NONE), Change::NONE);
    }

    /// A bitmap that stands for the window: what is copied to it stays.
    struct Canvas {
        dc: HDC,
        bitmap: HBITMAP,
        old: HGDIOBJ,
        size: (i32, i32),
    }

    impl Canvas {
        fn new(like: HDC, width: i32, height: i32) -> Self {
            // SAFETY: both are owned by the canvas until it is dropped.
            unsafe {
                let dc = CreateCompatibleDC(Some(like));
                let bitmap = CreateCompatibleBitmap(like, width, height);
                let old = SelectObject(dc, bitmap.into());
                Self {
                    dc,
                    bitmap,
                    old,
                    size: (width, height),
                }
            }
        }

        /// Every pixel, row by row.
        fn pixels(&self) -> Vec<u32> {
            use windows::Win32::Graphics::Gdi::{
                BI_RGB, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, GetDIBits,
            };
            let (width, height) = self.size;
            let mut pixels = vec![0u32; (width * height) as usize];
            let mut info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    // Top-down.
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            // SAFETY: the bitmap is selected out while it is read, as the
            // call demands, and `pixels` has room for every row asked for.
            unsafe {
                SelectObject(self.dc, self.old);
                let rows = GetDIBits(
                    self.dc,
                    self.bitmap,
                    0,
                    height as u32,
                    Some(pixels.as_mut_ptr().cast()),
                    &mut info,
                    DIB_RGB_COLORS,
                );
                SelectObject(self.dc, self.bitmap.into());
                assert_eq!(rows, height);
            }
            pixels
        }
    }

    impl Drop for Canvas {
        fn drop(&mut self) {
            // SAFETY: the bitmap is selected out before both are deleted.
            unsafe {
                SelectObject(self.dc, self.old);
                let _ = DeleteObject(self.bitmap.into());
                let _ = DeleteDC(self.dc);
            }
        }
    }

    /// Draws into bitmaps in memory only: no window is made or shown.
    #[test]
    fn what_is_copied_to_the_window_is_the_page_whatever_came_before() {
        use docklet_app::SharedLog;

        let fonts = Fonts::new(96, HWND::default());
        let theme = Theme::new(true);
        let (width, height) = (420, 230);
        let area = RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        };
        let page = u64::try_from(height / fonts.mono_line.max(1)).unwrap();
        assert!(page > 6, "the test needs a page of some rows");
        let short = |number: u64| format!("\x1b[32mINFO\x1b[0m line {number}");
        let hangul = |number: u64| format!("{number} \x1b[33m한글 로그\x1b[0m ㅋㅋ ok");
        let long = |number: u64| format!("{number} {}", "wide ".repeat(60));

        // Each step: lines that arrive, where the view then is (lines back
        // from the end, or following), the selection as lines back from
        // the end, and how far the view is scrolled sideways.
        struct Step {
            arrive: Vec<String>,
            back: u64,
            selection: Option<(u64, u64)>,
            sideways: i32,
        }
        let step = |arrive: Vec<String>, back: u64| Step {
            arrive,
            back,
            selection: None,
            sideways: 0,
        };
        let mut steps = vec![
            // Nothing yet, then a log shorter than the view that grows.
            step(Vec::new(), 0),
            step((0..3).map(short).collect(), 0),
            step((3..5).map(hangul).collect(), 0),
            // The page fills and starts to move.
            step((5..5 + page).map(short).collect(), 0),
            step((100..102).map(hangul).collect(), 0),
            // A line wider than the view scrolls in, and through the page.
            step(vec![long(200)], 0),
        ];
        steps.extend((0..page + 2).map(|index| step(vec![short(300 + index)], 0)));
        steps.extend([
            // Back three lines, forward one, back a page and more.
            step(Vec::new(), 3),
            step(Vec::new(), 2),
            step(Vec::new(), page + 4),
            // Lines pile up below a view that stays where it is.
            step((400..403).map(hangul).collect(), page + 7),
            // A selection, sideways, and back to the end.
            Step {
                arrive: Vec::new(),
                back: page + 7,
                selection: Some((page + 9, page + 8)),
                sideways: 0,
            },
            step(Vec::new(), page + 7),
            Step {
                arrive: Vec::new(),
                back: page + 7,
                selection: None,
                sideways: 40,
            },
            step(Vec::new(), 0),
            step(vec![long(500), short(501)], 0),
            step((600..600 + page - 1).map(hangul).collect(), 0),
            step((700..700 + 3 * page).map(short).collect(), 0),
        ]);

        // SAFETY: GDI objects made here and freed here; the screen's DC is
        // only what the bitmaps are made compatible with.
        unsafe {
            let screen = GetDC(None);
            let shown = Canvas::new(screen, width, height);
            let expected = Canvas::new(screen, width, height);
            let log = SharedLog::new();
            let mut frame = Frame::default();
            let mut end = 0u64;
            for (index, step) in steps.into_iter().enumerate() {
                end += step.arrive.len() as u64;
                log.append(step.arrive);
                let top = end.saturating_sub(page + step.back);
                let selection = step
                    .selection
                    .map(|(first, last)| (end.saturating_sub(first), end.saturating_sub(last)));
                let look = Look {
                    theme: &theme,
                    font: fonts.mono,
                    hangul: fonts.hangul,
                    line_height: fonts.mono_line,
                    ascent: fonts.mono_ascent,
                    left: 6 - step.sideways,
                };
                // The window: only what the frame says changed is copied.
                let changed = frame.update(screen, area, Some(&log), (top, end), selection, &look);
                if let Some(changed) = changed {
                    frame.blit(shown.dc, changed, area, None);
                }
                // The same page drawn from nothing.
                let mut fresh = Frame::default();
                fresh.update(screen, area, Some(&log), (top, end), selection, &look);
                fresh.blit(expected.dc, area, area, None);
                let (shown, expected) = (shown.pixels(), expected.pixels());
                let differs = shown.iter().zip(&expected).position(|(a, b)| a != b);
                let at = differs.map(|at| (at as i32 % width, at as i32 / width));
                assert_eq!(at, None, "step {index}: the window differs from the page");
            }
            ReleaseDC(None, screen);
        }
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
