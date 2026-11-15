//! The lines of a table as the list control asks for them, without any
//! drawing. Which lines there are, in which order and which are ticked is
//! decided outside the window (`docklet_app::ListModel`); this only reads
//! them.

use crate::boundary::{Line, Row, Table};

/// A line in terms that survive new data: its row's id, or its group. The
/// focus follows it from one table to the next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LineKey {
    Group(String),
    Row(String),
}

pub fn key_of(line: &Line) -> LineKey {
    match line {
        Line::Group { name, .. } => LineKey::Group(name.clone()),
        Line::Row(row) => LineKey::Row(row.id.clone()),
    }
}

pub fn keys(lines: &[Line]) -> Vec<LineKey> {
    lines.iter().map(key_of).collect()
}

pub fn row_at(table: &Table, index: usize) -> Option<&Row> {
    match table.lines.get(index)? {
        Line::Row(row) => Some(row),
        Line::Group { .. } => None,
    }
}

/// What a group header shows in the first column.
pub fn group_title(name: &str, count: usize, collapsed: bool) -> String {
    let arrow = if collapsed { '▸' } else { '▾' };
    format!("{arrow} {name} ({count})")
}

/// Whether a line's box is ticked; `None` for a line without a box.
pub fn check_of(line: &Line) -> Option<bool> {
    match line {
        Line::Group { check, .. } => *check,
        Line::Row(row) => row.check,
    }
}

/// The table's actions that have a button in the last column, by their
/// index in `Table::actions`, in the order of the buttons.
pub fn button_actions(table: &Table) -> Vec<usize> {
    let actions = table.actions.iter().enumerate();
    actions
        .filter(|(_, action)| action.button)
        .map(|(index, _)| index)
        .collect()
}

/// The first line from `start` on, wrapping, whose first cell (or group
/// name) starts with `prefix`, ignoring case: type-to-find.
pub fn find_prefix(lines: &[Line], prefix: &str, start: usize) -> Option<usize> {
    let prefix = prefix.to_lowercase();
    let count = lines.len();
    (0..count)
        .map(|step| (start + step) % count)
        .find(|&index| {
            let text = match &lines[index] {
                Line::Row(row) => row.cells.first().map_or("", String::as_str),
                Line::Group { name, .. } => name,
            };
            text.to_lowercase().starts_with(&prefix)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boundary::Action;

    fn group(name: &str) -> Line {
        Line::Group {
            name: name.to_string(),
            count: 2,
            collapsed: false,
            check: Some(false),
        }
    }

    fn row(id: &str, name: &str) -> Line {
        Line::Row(Row::new(id, vec![name.to_string()]))
    }

    fn lines() -> Vec<Line> {
        vec![
            group("shop"),
            row("a", "web-10"),
            row("b", "web-9"),
            group("blog"),
            row("d", "db"),
            row("c", "solo"),
            row("e", "한글-app"),
        ]
    }

    #[test]
    fn keys_find_a_line_again_in_the_next_table() {
        let before = keys(&lines());
        assert_eq!(before[0], LineKey::Group("shop".into()));
        assert_eq!(before[4], LineKey::Row("d".into()));
        // New data: a row goes, so "d" moves up a line.
        let mut after = lines();
        after.remove(1);
        let position = keys(&after).iter().position(|key| *key == before[4]);
        assert_eq!(position, Some(3));
    }

    #[test]
    fn rows_and_groups_are_found_by_prefix() {
        let lines = lines();
        assert_eq!(find_prefix(&lines, "WEB", 0), Some(1));
        assert_eq!(find_prefix(&lines, "web", 2), Some(2));
        assert_eq!(find_prefix(&lines, "web", 3), Some(1));
        assert_eq!(find_prefix(&lines, "blog", 0), Some(3));
        assert_eq!(find_prefix(&lines, "한", 0), Some(6));
        assert_eq!(find_prefix(&lines, "zzz", 0), None);
        assert_eq!(find_prefix(&[], "a", 0), None);
    }

    #[test]
    fn a_group_header_says_its_name_its_size_and_whether_it_is_open() {
        assert_eq!(group_title("shop", 2, false), "▾ shop (2)");
        assert_eq!(group_title("위키", 12, true), "▸ 위키 (12)");
    }

    #[test]
    fn boxes_and_rows_are_read_off_the_lines() {
        let mut ticked = Row::new("x", vec![]);
        ticked.check = Some(true);
        let mut fixed = Row::new("y", vec![]);
        fixed.check = None;
        let table = Table {
            lines: vec![group("g"), Line::Row(ticked), Line::Row(fixed)],
            ..Table::default()
        };
        let checks: Vec<Option<bool>> = table.lines.iter().map(check_of).collect();
        assert_eq!(checks, [Some(false), Some(true), None]);
        assert!(row_at(&table, 0).is_none());
        assert_eq!(row_at(&table, 1).map(|row| row.id.as_str()), Some("x"));
        assert!(row_at(&table, 9).is_none());
    }

    #[test]
    fn only_actions_with_a_button_get_a_slot() {
        let table = Table {
            actions: vec![
                Action::new("start", "Start"),
                Action::in_menu("restart", "Restart"),
                Action::new("remove", "Remove"),
            ],
            ..Table::default()
        };
        assert_eq!(button_actions(&table), [0, 2]);
    }
}
