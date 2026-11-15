//! What a table shows and which rows are checked, without any drawing.
//!
//! The list control asks for line `i`; [`TableState::entries`] says whether
//! that is a group header or which row of the [`Table`] it is. Rows are
//! remembered by id, so checks survive new data, sorting and filtering.

use std::cmp::Ordering;
use std::collections::HashSet;

use crate::boundary::{SortBy, Table};

/// One line of the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry {
    /// Header of group `.0`, an index into [`Table::groups`].
    Group(u32),
    /// Row `.0` of [`Table::rows`].
    Row(usize),
}

/// A line in terms that survive new data: its row's id, or its group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryKey {
    Group(u32),
    Row(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckState {
    None,
    Some,
    All,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sort {
    pub column: usize,
    pub ascending: bool,
}

#[derive(Debug, Default)]
pub struct TableState {
    filter: String,
    /// `filter`, lower-cased and trimmed.
    needle: String,
    sort: Option<Sort>,
    checked: HashSet<String>,
    /// Id of the row a Shift range starts from.
    anchor: Option<String>,
    collapsed: HashSet<u32>,
    entries: Vec<Entry>,
    /// What each entry shows, readable without the table it was built from.
    keys: Vec<EntryKey>,
    /// Rows that pass the filter, in display order, collapsed ones included.
    ordered: Vec<usize>,
    /// For each group, its members as a range of `ordered`.
    group_ranges: Vec<(usize, usize)>,
    /// Row and group counts of the table this was built from. Every index
    /// above is in bounds for a table of these counts.
    built_for: (usize, usize),
}

/// Compares text with runs of ASCII digits as numbers, ignoring case, so
/// that "web-10" sorts after "web-9".
pub fn natural_cmp(left: &str, right: &str) -> Ordering {
    let mut left = left.chars().peekable();
    let mut right = right.chars().peekable();
    loop {
        match (left.peek().copied(), right.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(a), Some(b)) if a.is_ascii_digit() && b.is_ascii_digit() => {
                let a = take_number(&mut left);
                let b = take_number(&mut right);
                // Longer runs without leading zeros are bigger numbers.
                let order = a
                    .trim_start_matches('0')
                    .len()
                    .cmp(&b.trim_start_matches('0').len())
                    .then_with(|| a.trim_start_matches('0').cmp(b.trim_start_matches('0')));
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(a), Some(b)) => {
                let order = a.to_lowercase().cmp(b.to_lowercase());
                if order != Ordering::Equal {
                    return order;
                }
                left.next();
                right.next();
            }
        }
    }
}

fn take_number(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut digits = String::new();
    while let Some(&digit) = chars.peek().filter(|c| c.is_ascii_digit()) {
        digits.push(digit);
        chars.next();
    }
    digits
}

impl TableState {
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// Takes the filter as typed. Call [`Self::rebuild`] afterwards.
    pub fn set_filter(&mut self, text: &str) {
        self.filter = text.to_string();
        self.needle = text.trim().to_lowercase();
    }

    pub fn sort(&self) -> Option<Sort> {
        self.sort
    }

    /// Sorts by `column`, the other way round if it already sorts by it.
    /// Call [`Self::rebuild`] afterwards.
    pub fn sort_by(&mut self, column: usize) -> Sort {
        let ascending = match self.sort {
            Some(sort) if sort.column == column => !sort.ascending,
            _ => true,
        };
        let sort = Sort { column, ascending };
        self.sort = Some(sort);
        sort
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn entry(&self, index: usize) -> Option<Entry> {
        self.entries.get(index).copied()
    }

    /// Rows that pass the filter.
    pub fn matching(&self) -> usize {
        self.ordered.len()
    }

    pub fn checked_count(&self) -> usize {
        self.checked.len()
    }

    pub fn is_collapsed(&self, group: u32) -> bool {
        self.collapsed.contains(&group)
    }

    /// Members of `group` that pass the filter.
    pub fn group_size(&self, group: u32) -> usize {
        self.group_ranges
            .get(group as usize)
            .map_or(0, |&(start, end)| end - start)
    }

    fn matches(&self, table: &Table, row: usize) -> bool {
        self.needle.is_empty()
            || table.rows[row]
                .cells
                .iter()
                .any(|cell| cell.to_lowercase().contains(&self.needle))
    }

    fn compare(&self, table: &Table, left: usize, right: usize) -> Ordering {
        let (left, right) = (&table.rows[left], &table.rows[right]);
        let Some(sort) = self.sort else {
            return Ordering::Equal;
        };
        let by = table
            .columns
            .get(sort.column)
            .map_or(SortBy::Unsorted, |column| column.sort);
        fn cell(row: &crate::boundary::Row, column: usize) -> &str {
            row.cells.get(column).map_or("", String::as_str)
        }
        let order = match by {
            SortBy::Unsorted => Ordering::Equal,
            SortBy::Text => natural_cmp(cell(left, sort.column), cell(right, sort.column)),
            SortBy::Key => {
                let key =
                    |row: &crate::boundary::Row| row.keys.get(sort.column).copied().unwrap_or(0);
                key(left).cmp(&key(right))
            }
        };
        if sort.ascending {
            order
        } else {
            order.reverse()
        }
    }

    /// Whether the lines were built from a table shaped like `table`, so
    /// that indexing it with them cannot go out of bounds. Callers check
    /// this before using a state with a table that may have been replaced.
    pub fn fits(&self, table: &Table) -> bool {
        self.built_for == (table.rows.len(), table.groups.len())
    }

    /// Recomputes the lines from `table`, and forgets checks of rows that
    /// are gone or filtered out. Returns whether the checked set changed.
    pub fn rebuild(&mut self, table: &Table) -> bool {
        self.built_for = (table.rows.len(), table.groups.len());
        let mut matching: Vec<usize> = (0..table.rows.len())
            .filter(|&row| self.matches(table, row))
            .collect();
        // Stable, so equal rows keep the order they came in.
        matching.sort_by(|&left, &right| self.compare(table, left, right));

        let groups = table.groups.len();
        let in_group = |row: usize| {
            table.rows[row]
                .group
                .filter(|&group| (group as usize) < groups)
        };
        self.entries.clear();
        self.ordered.clear();
        self.group_ranges.clear();
        for group in 0..groups as u32 {
            let start = self.ordered.len();
            self.ordered.extend(
                matching
                    .iter()
                    .copied()
                    .filter(|&row| in_group(row) == Some(group)),
            );
            let end = self.ordered.len();
            self.group_ranges.push((start, end));
            if start == end {
                continue;
            }
            self.entries.push(Entry::Group(group));
            if !self.collapsed.contains(&group) {
                self.entries
                    .extend(self.ordered[start..end].iter().map(|&row| Entry::Row(row)));
            }
        }
        for &row in &matching {
            if in_group(row).is_none() {
                self.ordered.push(row);
                self.entries.push(Entry::Row(row));
            }
        }
        self.keys = self
            .entries
            .iter()
            .map(|entry| match *entry {
                Entry::Group(group) => EntryKey::Group(group),
                Entry::Row(row) => EntryKey::Row(table.rows[row].id.clone()),
            })
            .collect();

        let before = self.checked.len();
        let visible: HashSet<&str> = self
            .ordered
            .iter()
            .map(|&row| table.rows[row].id.as_str())
            .collect();
        self.checked.retain(|id| visible.contains(id.as_str()));
        if let Some(anchor) = &self.anchor
            && !visible.contains(anchor.as_str())
        {
            self.anchor = None;
        }
        self.checked.len() != before
    }

    pub fn is_row_checked(&self, table: &Table, row: usize) -> bool {
        table
            .rows
            .get(row)
            .is_some_and(|row| self.checked.contains(&row.id))
    }

    fn members(&self, group: u32) -> &[usize] {
        self.group_ranges
            .get(group as usize)
            .map_or(&[][..], |&(start, end)| &self.ordered[start..end])
    }

    fn state_of(&self, table: &Table, rows: &[usize]) -> CheckState {
        let checked = rows
            .iter()
            .filter(|&&row| self.is_row_checked(table, row))
            .count();
        match checked {
            0 => CheckState::None,
            n if n == rows.len() => CheckState::All,
            _ => CheckState::Some,
        }
    }

    /// Whether the checkbox of line `index` shows a check. A group header
    /// does when all its members are checked.
    pub fn is_checked(&self, table: &Table, index: usize) -> bool {
        match self.entry(index) {
            Some(Entry::Row(row)) => self.is_row_checked(table, row),
            Some(Entry::Group(group)) => {
                self.state_of(table, self.members(group)) == CheckState::All
            }
            None => false,
        }
    }

    /// The select-all checkbox in the header.
    pub fn header_state(&self, table: &Table) -> CheckState {
        if self.ordered.is_empty() {
            return CheckState::None;
        }
        self.state_of(table, &self.ordered)
    }

    fn set_rows(&mut self, table: &Table, rows: &[usize], on: bool) {
        for &row in rows {
            let id = &table.rows[row].id;
            if on {
                if !self.checked.contains(id) {
                    self.checked.insert(id.clone());
                }
            } else {
                self.checked.remove(id);
            }
        }
    }

    /// Space or a click on a checkbox. A group header checks or unchecks all
    /// of its members. Returns whether anything changed.
    pub fn toggle(&mut self, table: &Table, index: usize) -> bool {
        match self.entry(index) {
            Some(Entry::Row(row)) => {
                let on = !self.is_row_checked(table, row);
                self.set_rows(table, &[row], on);
                self.anchor = Some(table.rows[row].id.clone());
                true
            }
            Some(Entry::Group(group)) => {
                let members = self.members(group).to_vec();
                let on = self.state_of(table, &members) != CheckState::All;
                self.set_rows(table, &members, on);
                !members.is_empty()
            }
            None => false,
        }
    }

    /// Shift with a click or Space: every row from the anchor to line `index`
    /// takes the state that line is toggled to.
    pub fn check_range(&mut self, table: &Table, index: usize) -> bool {
        let Some(Entry::Row(target)) = self.entry(index) else {
            return self.toggle(table, index);
        };
        let anchor = self
            .anchor
            .as_deref()
            .and_then(|id| self.index_of_row(id))
            .unwrap_or(index);
        let on = !self.is_row_checked(table, target);
        let (low, high) = (anchor.min(index), anchor.max(index));
        let rows: Vec<usize> = self.entries[low..=high]
            .iter()
            .filter_map(|entry| match entry {
                Entry::Row(row) => Some(*row),
                Entry::Group(_) => None,
            })
            .collect();
        self.set_rows(table, &rows, on);
        self.anchor = Some(table.rows[target].id.clone());
        true
    }

    /// Ctrl+A and the header checkbox: checks every row that passes the
    /// filter, or unchecks them all if they already are.
    pub fn toggle_all(&mut self, table: &Table) -> bool {
        if self.ordered.is_empty() {
            return false;
        }
        let on = self.header_state(table) != CheckState::All;
        let rows = self.ordered.clone();
        self.set_rows(table, &rows, on);
        true
    }

    pub fn clear(&mut self) -> bool {
        self.anchor = None;
        let had = !self.checked.is_empty();
        self.checked.clear();
        had
    }

    /// The checked rows' ids in display order.
    pub fn checked_ids(&self, table: &Table) -> Vec<String> {
        self.ordered
            .iter()
            .map(|&row| &table.rows[row].id)
            .filter(|id| self.checked.contains(*id))
            .cloned()
            .collect()
    }

    /// Collapses or expands a group. Call [`Self::rebuild`] afterwards.
    pub fn set_collapsed(&mut self, group: u32, collapsed: bool) {
        if collapsed {
            self.collapsed.insert(group);
        } else {
            self.collapsed.remove(&group);
        }
    }

    /// What line `index` shows. Needs no table, so it still answers after
    /// the table it was built from has been replaced.
    pub fn key_at(&self, index: usize) -> Option<&EntryKey> {
        self.keys.get(index)
    }

    /// The line that shows `key`.
    pub fn index_of(&self, key: &EntryKey) -> Option<usize> {
        self.keys.iter().position(|candidate| candidate == key)
    }

    /// The line that shows the row with `id`.
    pub fn index_of_row(&self, id: &str) -> Option<usize> {
        self.keys
            .iter()
            .position(|key| matches!(key, EntryKey::Row(row) if row == id))
    }

    pub fn index_of_group(&self, group: u32) -> Option<usize> {
        self.index_of(&EntryKey::Group(group))
    }

    /// The first line from `start` on, wrapping, whose first cell (or group
    /// title) starts with `prefix`, ignoring case: type-to-find.
    pub fn find_prefix(&self, table: &Table, prefix: &str, start: usize) -> Option<usize> {
        let prefix = prefix.to_lowercase();
        let count = self.entries.len();
        (0..count)
            .map(|step| (start + step) % count)
            .find(|&index| {
                let text = match self.entries[index] {
                    Entry::Row(row) => table.rows[row].cells.first().map_or("", String::as_str),
                    Entry::Group(group) => {
                        table.groups.get(group as usize).map_or("", String::as_str)
                    }
                };
                text.to_lowercase().starts_with(&prefix)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boundary::{Column, Row};

    fn table() -> Table {
        let row = |id: &str, name: &str, size: i64, group: Option<u32>| {
            let mut row = Row::new(id, vec![name.to_string(), format!("{size} MB")]);
            row.keys = vec![0, size];
            row.group = group;
            row
        };
        Table {
            columns: vec![
                Column::new("Name", 2),
                Column::new("Size", 1).sort(SortBy::Key),
            ],
            groups: vec!["shop".into(), "blog".into()],
            rows: vec![
                row("a", "web-10", 300, Some(0)),
                row("b", "web-9", 20, Some(0)),
                row("c", "solo", 5, None),
                row("d", "db", 900, Some(1)),
                row("e", "한글-app", 1, None),
            ],
            ..Table::default()
        }
    }

    fn built(table: &Table) -> TableState {
        let mut state = TableState::default();
        state.rebuild(table);
        state
    }

    #[test]
    fn natural_order_compares_numbers_as_numbers() {
        assert_eq!(natural_cmp("web-9", "web-10"), Ordering::Less);
        assert_eq!(natural_cmp("Web-2", "web-02"), Ordering::Equal);
        assert_eq!(natural_cmp("a", "B"), Ordering::Less);
        assert_eq!(natural_cmp("x1", "x"), Ordering::Greater);
        assert_eq!(natural_cmp("8080:80", "443:443"), Ordering::Greater);
    }

    #[test]
    fn groups_come_first_with_headers_then_ungrouped_rows() {
        let table = table();
        let state = built(&table);
        assert_eq!(
            state.entries(),
            &[
                Entry::Group(0),
                Entry::Row(0),
                Entry::Row(1),
                Entry::Group(1),
                Entry::Row(3),
                Entry::Row(2),
                Entry::Row(4),
            ]
        );
        assert_eq!(state.group_size(0), 2);
        assert_eq!(state.matching(), 5);
    }

    #[test]
    fn sorting_applies_within_groups_and_toggles_direction() {
        let table = table();
        let mut state = built(&table);
        state.sort_by(0);
        state.rebuild(&table);
        assert_eq!(&state.entries()[1..3], &[Entry::Row(1), Entry::Row(0)]);
        assert!(!state.sort_by(0).ascending);
        state.rebuild(&table);
        assert_eq!(&state.entries()[1..3], &[Entry::Row(0), Entry::Row(1)]);
        state.sort_by(1);
        state.rebuild(&table);
        assert_eq!(&state.entries()[5..], &[Entry::Row(4), Entry::Row(2)]);
    }

    #[test]
    fn the_filter_matches_any_cell_and_hides_empty_groups() {
        let table = table();
        let mut state = built(&table);
        state.set_filter("  WEB ");
        state.rebuild(&table);
        assert_eq!(
            state.entries(),
            &[Entry::Group(0), Entry::Row(0), Entry::Row(1)]
        );
        state.set_filter("900 mb");
        state.rebuild(&table);
        assert_eq!(state.entries(), &[Entry::Group(1), Entry::Row(3)]);
        state.set_filter("한글");
        state.rebuild(&table);
        assert_eq!(state.entries(), &[Entry::Row(4)]);
    }

    #[test]
    fn collapsing_hides_members_but_keeps_them_for_the_group_checkbox() {
        let table = table();
        let mut state = built(&table);
        state.set_collapsed(0, true);
        state.rebuild(&table);
        assert_eq!(state.entries()[..2], [Entry::Group(0), Entry::Group(1)]);
        assert!(state.toggle(&table, 0));
        assert_eq!(state.checked_ids(&table), vec!["a", "b"]);
        assert!(state.is_checked(&table, 0));
        assert_eq!(state.header_state(&table), CheckState::Some);
    }

    #[test]
    fn toggling_rows_and_groups() {
        let table = table();
        let mut state = built(&table);
        assert!(state.toggle(&table, 1));
        assert!(state.is_checked(&table, 1));
        assert!(!state.is_checked(&table, 0));
        // The group is partly checked, so its checkbox checks the rest.
        state.toggle(&table, 0);
        assert_eq!(state.checked_ids(&table), vec!["a", "b"]);
        state.toggle(&table, 0);
        assert_eq!(state.checked_count(), 0);
        assert!(!state.toggle(&table, 99));
    }

    #[test]
    fn shift_ranges_run_from_the_anchor_and_skip_headers() {
        let table = table();
        let mut state = built(&table);
        state.toggle(&table, 1);
        state.check_range(&table, 5);
        assert_eq!(state.checked_ids(&table), vec!["a", "b", "d", "c"]);
        // The anchor moved to line 5; a range back up unchecks, as line 2
        // is checked.
        state.check_range(&table, 2);
        assert_eq!(state.checked_ids(&table), vec!["a"]);
    }

    #[test]
    fn a_range_without_an_anchor_is_a_toggle() {
        let table = table();
        let mut state = built(&table);
        state.check_range(&table, 6);
        assert_eq!(state.checked_ids(&table), vec!["e"]);
    }

    #[test]
    fn select_all_toggles_and_drives_the_header_checkbox() {
        let table = table();
        let mut state = built(&table);
        assert_eq!(state.header_state(&table), CheckState::None);
        state.toggle_all(&table);
        assert_eq!(state.header_state(&table), CheckState::All);
        assert_eq!(state.checked_count(), 5);
        state.toggle_all(&table);
        assert_eq!(state.checked_count(), 0);
        state.toggle(&table, 1);
        state.toggle_all(&table);
        assert_eq!(state.checked_count(), 5);
    }

    #[test]
    fn checks_follow_ids_and_drop_rows_that_go_or_are_filtered_out() {
        let mut table = table();
        let mut state = built(&table);
        state.toggle_all(&table);
        table.rows.remove(0);
        assert!(state.rebuild(&table));
        assert_eq!(state.checked_count(), 4);
        state.set_filter("db");
        assert!(state.rebuild(&table));
        assert_eq!(state.checked_ids(&table), vec!["d"]);
        assert!(!state.rebuild(&table));
        assert!(state.clear());
        assert!(!state.clear());
    }

    #[test]
    fn rows_and_groups_are_found_by_id_and_prefix() {
        let table = table();
        let state = built(&table);
        assert_eq!(state.index_of_row("d"), Some(4));
        assert_eq!(state.index_of_group(1), Some(3));
        assert_eq!(state.find_prefix(&table, "WEB", 0), Some(1));
        assert_eq!(state.find_prefix(&table, "web", 2), Some(2));
        assert_eq!(state.find_prefix(&table, "web", 3), Some(1));
        assert_eq!(state.find_prefix(&table, "blog", 0), Some(3));
        assert_eq!(state.find_prefix(&table, "zzz", 0), None);
    }

    #[test]
    fn keys_outlive_the_table_and_find_the_line_in_the_next_one() {
        let mut table = table();
        let mut state = built(&table);
        let key = state.key_at(4).cloned();
        assert_eq!(key, Some(EntryKey::Row("d".into())));
        assert_eq!(state.key_at(0), Some(&EntryKey::Group(0)));
        // New data: a row goes, so "d" moves up a line.
        table.rows.remove(0);
        assert!(!state.fits(&table));
        state.rebuild(&table);
        assert!(state.fits(&table));
        assert_eq!(state.index_of(&key.unwrap()), Some(3));
    }

    #[test]
    fn a_state_fits_only_tables_of_the_shape_it_was_built_from() {
        let mut table = table();
        let state = built(&table);
        assert!(state.fits(&table));
        table.groups.push("extra".into());
        assert!(!state.fits(&table));
        assert!(TableState::default().fits(&Table::default()));
    }
}
