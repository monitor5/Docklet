//! Which rows are selected, by row ID.

use std::collections::HashSet;

/// How much of what is visible is selected, for a "select all" box.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Coverage {
    #[default]
    None,
    Some,
    All,
}

/// A set of selected row IDs, with the row a range starts from.
///
/// It knows nothing about tables: whoever owns the rows says which ones are
/// visible, and drops what vanished with `retain` after a reload.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    ids: HashSet<String>,
    /// The row last toggled, where a range starts.
    anchor: Option<String>,
}

impl Selection {
    pub fn contains(&self, id: &str) -> bool {
        self.ids.contains(id)
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Selects the row if it was not selected and the other way round.
    /// Returns whether it is selected now.
    pub fn toggle(&mut self, id: &str) -> bool {
        self.anchor = Some(id.to_string());
        if self.ids.remove(id) {
            false
        } else {
            self.ids.insert(id.to_string());
            true
        }
    }

    /// Selects every row from the one last toggled to `id`, both included,
    /// in the order of `visible` (a click with Shift held). Without a row
    /// to start from, or when either row is not visible, selects `id` alone.
    pub fn select_range(&mut self, visible: &[&str], id: &str) {
        let position = |wanted: &str| visible.iter().position(|row| *row == wanted);
        let start = self.anchor.as_deref().and_then(position);
        match (start, position(id)) {
            (Some(start), Some(end)) => {
                let range = start.min(end)..=start.max(end);
                self.ids
                    .extend(visible[range].iter().map(|row| row.to_string()));
            }
            _ => {
                self.ids.insert(id.to_string());
                self.anchor = Some(id.to_string());
            }
        }
    }

    /// Selects every row of `visible`, on top of what is selected.
    pub fn select_all<'a>(&mut self, visible: impl IntoIterator<Item = &'a str>) {
        self.ids.extend(visible.into_iter().map(str::to_string));
    }

    pub fn clear(&mut self) {
        self.ids.clear();
        self.anchor = None;
    }

    /// Drops every selected row `keep` turns down: the ones a reload no
    /// longer lists, or a filter no longer shows.
    pub fn retain(&mut self, mut keep: impl FnMut(&str) -> bool) {
        self.ids.retain(|id| keep(id));
        if self.anchor.as_deref().is_some_and(|anchor| !keep(anchor)) {
            self.anchor = None;
        }
    }

    /// The selected rows among `order`, in that order.
    pub fn in_order<'a>(&self, order: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
        order.into_iter().filter(|id| self.contains(id)).collect()
    }

    /// How many of `visible` are selected: none, some or all of them.
    pub fn coverage<'a>(&self, visible: impl IntoIterator<Item = &'a str>) -> Coverage {
        let (mut total, mut selected) = (0usize, 0usize);
        for id in visible {
            total += 1;
            selected += usize::from(self.contains(id));
        }
        match selected {
            0 => Coverage::None,
            _ if selected == total => Coverage::All,
            _ => Coverage::Some,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROWS: [&str; 5] = ["a", "b", "c", "d", "e"];

    fn selected(selection: &Selection) -> Vec<&'static str> {
        selection.in_order(ROWS)
    }

    #[test]
    fn toggling_selects_and_deselects() {
        let mut selection = Selection::default();
        assert!(selection.toggle("b"));
        assert!(selection.toggle("d"));
        assert_eq!(selected(&selection), ["b", "d"]);
        assert_eq!(selection.len(), 2);
        assert!(!selection.toggle("b"));
        assert_eq!(selected(&selection), ["d"]);
        assert!(selection.contains("d"));
        assert!(!selection.contains("b"));
    }

    #[test]
    fn a_range_runs_from_the_last_toggled_row_in_either_direction() {
        let mut selection = Selection::default();
        selection.toggle("b");
        selection.select_range(&ROWS, "d");
        assert_eq!(selected(&selection), ["b", "c", "d"]);

        let mut upwards = Selection::default();
        upwards.toggle("d");
        upwards.select_range(&ROWS, "a");
        assert_eq!(selected(&upwards), ["a", "b", "c", "d"]);
    }

    #[test]
    fn a_range_follows_the_visible_order_not_the_alphabet() {
        let mut selection = Selection::default();
        selection.toggle("e");
        selection.select_range(&["e", "a", "c", "b"], "c");
        assert_eq!(selected(&selection), ["a", "c", "e"]);
    }

    #[test]
    fn a_range_adds_to_what_is_selected_and_can_be_extended() {
        let mut selection = Selection::default();
        selection.toggle("a");
        selection.toggle("c");
        selection.select_range(&ROWS, "d");
        assert_eq!(selected(&selection), ["a", "c", "d"]);
        // The range still starts at `c`.
        selection.select_range(&ROWS, "e");
        assert_eq!(selected(&selection), ["a", "c", "d", "e"]);
    }

    #[test]
    fn a_range_without_a_start_selects_the_one_row() {
        let mut selection = Selection::default();
        selection.select_range(&ROWS, "c");
        assert_eq!(selected(&selection), ["c"]);
        // And that row is where the next range starts.
        selection.select_range(&ROWS, "e");
        assert_eq!(selected(&selection), ["c", "d", "e"]);
    }

    #[test]
    fn a_range_from_a_row_that_is_not_visible_selects_the_one_row() {
        let mut selection = Selection::default();
        selection.toggle("a");
        selection.select_range(&["c", "d", "e"], "e");
        assert_eq!(selected(&selection), ["a", "e"]);
    }

    #[test]
    fn select_all_takes_the_visible_rows_only() {
        let mut selection = Selection::default();
        selection.select_all(["b", "c"]);
        assert_eq!(selected(&selection), ["b", "c"]);
        assert_eq!(selection.coverage(["b", "c"]), Coverage::All);
        assert_eq!(selection.coverage(ROWS), Coverage::Some);
    }

    #[test]
    fn clear_empties_the_selection_and_forgets_the_range_start() {
        let mut selection = Selection::default();
        selection.toggle("b");
        selection.clear();
        assert!(selection.is_empty());
        selection.select_range(&ROWS, "d");
        assert_eq!(selected(&selection), ["d"]);
    }

    #[test]
    fn rows_that_vanished_drop_out_after_a_reload() {
        let mut selection = Selection::default();
        selection.select_all(ROWS);
        selection.toggle("e");
        selection.toggle("e");
        // The reload no longer lists `b` and `e`.
        let listed = ["a", "c", "d", "f"];
        selection.retain(|id| listed.contains(&id));
        assert_eq!(selection.len(), 3);
        assert_eq!(selection.in_order(listed), ["a", "c", "d"]);
        // `e` was where a range would have started.
        selection.select_range(&listed, "f");
        assert_eq!(selection.in_order(listed), ["a", "c", "d", "f"]);
    }

    #[test]
    fn coverage_says_none_some_or_all() {
        let mut selection = Selection::default();
        assert_eq!(selection.coverage(ROWS), Coverage::None);
        selection.toggle("a");
        assert_eq!(selection.coverage(ROWS), Coverage::Some);
        selection.select_all(ROWS);
        assert_eq!(selection.coverage(ROWS), Coverage::All);
        // Nothing visible: nothing to tick.
        assert_eq!(selection.coverage([]), Coverage::None);
    }
}
