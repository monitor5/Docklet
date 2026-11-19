//! Where the log view looks and what is selected, without any drawing.
//!
//! Lines are addressed by absolute number: the source keeps counting when old
//! lines fall out of its ring, so a position stays meaningful while the ring
//! moves underneath it.

/// The follow-tail state machine.
///
/// Following: the view shows the newest lines and moves with every batch.
/// Paused: the view stays on the lines it shows (until they fall out of the
/// ring) and lines pile up below it. Scrolling to the bottom or jumping to
/// the end follows again; scrolling up, or starting a selection, pauses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogState {
    first: u64,
    end: u64,
    top: u64,
    page: u64,
    follow: bool,
    selection: Option<Selection>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Selection {
    anchor: u64,
    caret: u64,
}

impl Default for LogState {
    fn default() -> Self {
        Self::new()
    }
}

impl LogState {
    pub fn new() -> Self {
        Self {
            first: 0,
            end: 0,
            top: 0,
            page: 1,
            follow: true,
            selection: None,
        }
    }

    pub fn top(&self) -> u64 {
        self.top
    }

    pub fn first(&self) -> u64 {
        self.first
    }

    pub fn end(&self) -> u64 {
        self.end
    }

    pub fn len(&self) -> u64 {
        self.end - self.first
    }

    pub fn is_empty(&self) -> bool {
        self.end == self.first
    }

    pub fn page(&self) -> u64 {
        self.page
    }

    pub fn following(&self) -> bool {
        self.follow
    }

    /// The top line that shows the newest line at the bottom.
    pub fn last_top(&self) -> u64 {
        self.end.saturating_sub(self.page).max(self.first)
    }

    /// Lines below the view.
    pub fn below(&self) -> u64 {
        self.end.saturating_sub(self.top + self.page)
    }

    /// The source now holds lines `first..end`.
    pub fn set_bounds(&mut self, first: u64, end: u64) {
        self.first = first;
        self.end = end.max(first);
        if let Some(selection) = &mut self.selection {
            if self.end == self.first
                || selection.anchor.max(selection.caret) < self.first
                || selection.anchor.min(selection.caret) >= self.end
            {
                self.selection = None;
            } else {
                let last = self.end - 1;
                selection.anchor = selection.anchor.clamp(self.first, last);
                selection.caret = selection.caret.clamp(self.first, last);
            }
        }
        self.settle();
    }

    /// The view now has room for `page` lines.
    pub fn set_page(&mut self, page: u64) {
        self.page = page.max(1);
        self.settle();
    }

    pub fn scroll_by(&mut self, lines: i64) {
        self.scroll_to(self.top.saturating_add_signed(lines));
    }

    /// Scrolls so that `line` is at the top, as far as there are lines.
    /// Reaching the bottom follows again; anything above it pauses.
    pub fn scroll_to(&mut self, line: u64) {
        let last_top = self.last_top();
        self.top = line.clamp(self.first, last_top);
        self.follow = self.top >= last_top;
    }

    pub fn jump_to_end(&mut self) {
        self.follow = true;
        self.settle();
    }

    /// Stops following without moving, as a selection starts.
    pub fn pause(&mut self) {
        self.follow = false;
    }

    fn settle(&mut self) {
        let last_top = self.last_top();
        self.top = if self.follow {
            last_top
        } else {
            self.top.clamp(self.first, last_top)
        };
    }

    /// The line at row `row` of the view, kept within the lines held.
    pub fn line_at_row(&self, row: i64) -> Option<u64> {
        if self.is_empty() {
            return None;
        }
        Some(
            self.top
                .saturating_add_signed(row)
                .clamp(self.first, self.end - 1),
        )
    }

    /// Starts a selection at `line`, or extends the current one to it.
    pub fn select_from(&mut self, line: u64, extend: bool) {
        self.pause();
        match &mut self.selection {
            Some(selection) if extend => selection.caret = line,
            _ => {
                self.selection = Some(Selection {
                    anchor: line,
                    caret: line,
                });
            }
        }
    }

    /// Moves the moving end of the selection, as a drag does.
    pub fn select_to(&mut self, line: u64) {
        if let Some(selection) = &mut self.selection {
            selection.caret = line;
        }
    }

    pub fn select_all(&mut self) {
        if !self.is_empty() {
            self.pause();
            self.selection = Some(Selection {
                anchor: self.first,
                caret: self.end - 1,
            });
        }
    }

    /// The selected lines, first and last inclusive.
    pub fn selection(&self) -> Option<(u64, u64)> {
        self.selection.map(|selection| {
            (
                selection.anchor.min(selection.caret),
                selection.anchor.max(selection.caret),
            )
        })
    }

    /// The selection's moving end, which a drag keeps in view.
    pub fn caret(&self) -> Option<u64> {
        self.selection.map(|selection| selection.caret)
    }

    /// Scrolls just enough to show `line`.
    pub fn reveal(&mut self, line: u64) {
        if line < self.top {
            self.scroll_to(line);
        } else if line >= self.top + self.page {
            self.scroll_to(line + 1 - self.page.min(line + 1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(page: u64, end: u64) -> LogState {
        let mut state = LogState::new();
        state.set_page(page);
        state.set_bounds(0, end);
        state
    }

    #[test]
    fn following_keeps_the_newest_line_at_the_bottom() {
        let mut state = view(10, 100);
        assert!(state.following());
        assert_eq!(state.top(), 90);
        state.set_bounds(0, 250);
        assert_eq!(state.top(), 240);
        assert_eq!(state.below(), 0);
    }

    #[test]
    fn a_short_log_starts_at_its_first_line() {
        let state = view(10, 4);
        assert_eq!(state.top(), 0);
        assert!(state.following());
    }

    #[test]
    fn scrolling_up_pauses_and_new_lines_pile_up_below() {
        let mut state = view(10, 100);
        state.scroll_by(-5);
        assert!(!state.following());
        assert_eq!(state.top(), 85);
        state.set_bounds(0, 200);
        assert_eq!(state.top(), 85);
        assert_eq!(state.below(), 105);
    }

    #[test]
    fn scrolling_back_to_the_bottom_follows_again() {
        let mut state = view(10, 100);
        state.scroll_by(-5);
        state.scroll_by(50);
        assert!(state.following());
        assert_eq!(state.top(), 90);
        state.scroll_to(0);
        state.scroll_to(u64::MAX);
        assert!(state.following());
    }

    #[test]
    fn jumping_to_the_end_follows_again() {
        let mut state = view(10, 100);
        state.scroll_to(3);
        state.set_bounds(0, 500);
        state.jump_to_end();
        assert!(state.following());
        assert_eq!(state.top(), 490);
    }

    #[test]
    fn a_paused_view_is_pushed_along_when_its_lines_fall_out_of_the_ring() {
        let mut state = view(10, 100);
        state.scroll_to(20);
        state.set_bounds(50, 150);
        assert_eq!(state.top(), 50);
        assert!(!state.following());
    }

    #[test]
    fn a_cleared_source_resets_the_view() {
        let mut state = view(10, 100);
        state.scroll_to(20);
        state.set_bounds(100, 100);
        assert_eq!(state.top(), 100);
        assert!(state.line_at_row(0).is_none());
    }

    #[test]
    fn resizing_while_following_stays_at_the_bottom() {
        let mut state = view(10, 100);
        state.set_page(30);
        assert_eq!(state.top(), 70);
        state.set_page(0);
        assert_eq!(state.page(), 1);
        assert_eq!(state.top(), 99);
    }

    #[test]
    fn selecting_pauses_and_spans_both_directions() {
        let mut state = view(10, 100);
        state.select_from(95, false);
        assert!(!state.following());
        state.select_to(91);
        assert_eq!(state.selection(), Some((91, 95)));
        state.select_from(99, true);
        assert_eq!(state.selection(), Some((95, 99)));
        state.select_from(10, false);
        assert_eq!(state.selection(), Some((10, 10)));
    }

    #[test]
    fn a_selection_follows_the_ring_and_vanishes_with_its_lines() {
        let mut state = view(10, 100);
        state.select_from(40, false);
        state.select_to(60);
        state.set_bounds(50, 150);
        assert_eq!(state.selection(), Some((50, 60)));
        state.set_bounds(61, 161);
        assert_eq!(state.selection(), None);
    }

    #[test]
    fn select_all_covers_what_the_source_holds() {
        let mut state = view(10, 100);
        state.set_bounds(30, 100);
        state.select_all();
        assert_eq!(state.selection(), Some((30, 99)));
        let mut empty = view(10, 0);
        empty.select_all();
        assert_eq!(empty.selection(), None);
    }

    #[test]
    fn rows_map_to_lines_within_the_ring() {
        let state = view(10, 100);
        assert_eq!(state.line_at_row(0), Some(90));
        assert_eq!(state.line_at_row(9), Some(99));
        assert_eq!(state.line_at_row(50), Some(99));
        assert_eq!(state.line_at_row(-200), Some(0));
    }

    #[test]
    fn revealing_a_line_scrolls_the_least() {
        let mut state = view(10, 100);
        state.reveal(50);
        assert_eq!(state.top(), 50);
        state.reveal(65);
        assert_eq!(state.top(), 56);
        state.reveal(60);
        assert_eq!(state.top(), 56);
    }
}
