//! One list as a window shows it: a table with its filter, its sort order,
//! its collapsed groups and its selection.
//!
//! A `ListModel` lives on the UI thread. It does no I/O: the lists arrive
//! as messages and are put in with `set_table`.

use std::collections::HashSet;

use docklet_engine::{ContainerUsage, ResourceKind};

use crate::removal::RemovalPlan;
use crate::selection::{Coverage, Selection};
use crate::table::{Column, Filter, Row, Sort, SortDirection, Table, container_column};

/// One line of a list: the header of a Compose project, or a row.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Item<'a> {
    Group {
        name: &'a str,
        /// Rows of the group that match the filter, shown or not.
        count: usize,
        /// Its rows are hidden.
        collapsed: bool,
    },
    Row(&'a Row),
}

enum Entry {
    Group {
        name: String,
        count: usize,
    },
    /// Index into the table's rows.
    Row(usize),
}

pub struct ListModel {
    table: Table,
    filter_text: String,
    filter: Filter,
    sort: Option<Sort>,
    collapsed: HashSet<String>,
    entries: Vec<Entry>,
    /// Rows that match the filter, in a collapsed group or not.
    matching: usize,
    selection: Selection,
    busy: HashSet<String>,
    generation: u64,
}

impl ListModel {
    /// An empty list of `kind`.
    pub fn new(kind: ResourceKind) -> Self {
        Self {
            table: Table::empty(kind),
            filter_text: String::new(),
            filter: Filter::default(),
            sort: None,
            collapsed: HashSet::new(),
            entries: Vec::new(),
            matching: 0,
            selection: Selection::default(),
            busy: HashSet::new(),
            generation: 0,
        }
    }

    pub fn kind(&self) -> ResourceKind {
        self.table.kind
    }

    pub fn columns(&self) -> &[Column] {
        &self.table.columns
    }

    /// Changes whenever what the list shows may have changed: repaint when
    /// it differs from the one last painted.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Puts in a freshly loaded table. Filter, sort order and collapsed
    /// groups stay; selected rows that the table no longer has drop out of
    /// the selection.
    pub fn set_table(&mut self, table: Table) {
        if table.kind != self.table.kind {
            // Another kind of list: its columns and row IDs mean something
            // else, so nothing of the old view applies. The filter is text
            // and stays.
            self.sort = None;
            self.collapsed.clear();
            self.selection.clear();
        }
        self.table = table;
        let table = &self.table;
        self.busy.retain(|id| table.row(id).is_some());
        self.rebuild();
    }

    /// Puts the containers' usage into the usage columns (see
    /// `Table::apply_usage`). Sorts again if the list is sorted by one of
    /// them.
    pub fn set_usage(&mut self, usage: Option<&[ContainerUsage]>) {
        if self.table.kind != ResourceKind::Container {
            return;
        }
        self.table.apply_usage(usage);
        if self
            .sort
            .is_some_and(|sort| sort.column >= container_column::CPU)
        {
            self.rebuild();
        } else {
            self.generation += 1;
        }
    }

    pub fn filter(&self) -> &str {
        &self.filter_text
    }

    /// Shows only the rows that match `text` (see `Filter`). Selected rows
    /// that no longer show are deselected: what is removed is what is seen.
    pub fn set_filter(&mut self, text: &str) {
        if text == self.filter_text {
            return;
        }
        self.filter_text = text.to_string();
        self.filter = Filter::new(text);
        self.rebuild();
    }

    pub fn sort(&self) -> Option<Sort> {
        self.sort
    }

    pub fn set_sort(&mut self, sort: Option<Sort>) {
        self.sort = sort;
        self.rebuild();
    }

    /// A click on a column header: ascending, then descending, then the
    /// default order again.
    pub fn toggle_sort(&mut self, column: usize) {
        let next = match self.sort {
            Some(sort) if sort.column == column => match sort.direction {
                SortDirection::Ascending => Some(Sort {
                    column,
                    direction: SortDirection::Descending,
                }),
                SortDirection::Descending => None,
            },
            _ => Some(Sort {
                column,
                direction: SortDirection::Ascending,
            }),
        };
        self.set_sort(next);
    }

    /// Hides the rows of a group, or shows them again. Hidden rows are
    /// deselected.
    pub fn toggle_group(&mut self, name: &str) {
        if !self.collapsed.remove(name) {
            self.collapsed.insert(name.to_string());
        }
        self.rebuild();
    }

    /// The number of lines: group headers and the rows that show.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The line at `index`, from the top.
    pub fn item(&self, index: usize) -> Option<Item<'_>> {
        Some(match self.entries.get(index)? {
            Entry::Group { name, count } => Item::Group {
                name,
                count: *count,
                collapsed: self.collapsed.contains(name),
            },
            Entry::Row(row) => Item::Row(&self.table.rows[*row]),
        })
    }

    /// The row with this ID, shown or not.
    pub fn row(&self, id: &str) -> Option<&Row> {
        self.table.row(id)
    }

    /// The number of rows the list has, whatever the filter.
    pub fn total(&self) -> usize {
        self.table.rows.len()
    }

    /// The number of rows that match the filter, those of collapsed groups
    /// included.
    pub fn matching(&self) -> usize {
        self.matching
    }

    /// What to show in place of a list without lines.
    pub fn empty_text(&self) -> &'static str {
        if self.table.rows.is_empty() {
            self.table.empty_text()
        } else {
            "Nothing matches the filter."
        }
    }

    /// Marks a row as having an action in flight, so its buttons can be
    /// disabled until `ActionDone` arrives.
    pub fn set_busy(&mut self, id: &str, busy: bool) {
        let changed = if busy {
            self.busy.insert(id.to_string())
        } else {
            self.busy.remove(id)
        };
        self.generation += u64::from(changed);
    }

    pub fn is_busy(&self, id: &str) -> bool {
        self.busy.contains(id)
    }

    pub fn is_selected(&self, id: &str) -> bool {
        self.selection.contains(id)
    }

    pub fn selected_count(&self) -> usize {
        self.selection.len()
    }

    /// Whether none, some or all of the rows that can be selected are, for
    /// the "select all" box.
    pub fn coverage(&self) -> Coverage {
        self.selection
            .coverage(selectable(&self.table, &self.entries))
    }

    /// Selects or deselects a row (a click on its box). Rows that do not
    /// show or cannot be removed cannot be selected.
    pub fn toggle(&mut self, id: &str) {
        if selectable(&self.table, &self.entries).contains(&id) {
            self.selection.toggle(id);
            self.generation += 1;
        }
    }

    /// Selects from the row last toggled to this one (a click with Shift).
    pub fn select_range_to(&mut self, id: &str) {
        let rows = selectable(&self.table, &self.entries);
        if rows.contains(&id) {
            self.selection.select_range(&rows, id);
            self.generation += 1;
        }
    }

    /// Selects every row that shows and can be removed: not the rows a
    /// filter or a collapsed group hides.
    pub fn select_all(&mut self) {
        self.selection
            .select_all(selectable(&self.table, &self.entries));
        self.generation += 1;
    }

    /// The "select all" box was clicked: selects every row that shows, or
    /// deselects everything when they all are already.
    pub fn toggle_all(&mut self) {
        if self.coverage() == Coverage::All {
            self.clear_selection();
        } else {
            self.select_all();
        }
    }

    /// Whether none, some or all of the rows that show under the group's
    /// header are selected. `None` for a collapsed group: it shows no rows,
    /// so there is nothing to tick.
    pub fn group_coverage(&self, name: &str) -> Coverage {
        self.selection.coverage(self.group_rows(name))
    }

    /// A click on a group's box: selects the rows that show under its
    /// header, or deselects them when they all are already. Does nothing to
    /// a collapsed group, whose rows are hidden.
    pub fn toggle_group_selection(&mut self, name: &str) {
        let all = self.group_coverage(name) == Coverage::All;
        let rows: Vec<String> = self.group_rows(name).map(str::to_string).collect();
        for id in &rows {
            if self.selection.contains(id) == all {
                self.selection.toggle(id);
            }
        }
        self.generation += u64::from(!rows.is_empty());
    }

    /// The IDs of the group's rows that show and can be removed.
    fn group_rows<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> {
        self.shown_rows()
            .filter(move |row| row.group.as_deref() == Some(name) && row.flags.removable)
            .map(|row| row.id.as_str())
    }

    pub fn clear_selection(&mut self) {
        self.selection.clear();
        self.generation += 1;
    }

    /// The selected rows as a removal, in the order they show.
    pub fn removal_plan(&self) -> RemovalPlan {
        let rows = self.shown_rows();
        let selected = rows.filter(|row| self.selection.contains(&row.id));
        RemovalPlan::new(self.table.kind, selected)
    }

    fn shown_rows(&self) -> impl Iterator<Item = &Row> {
        shown_rows(&self.table, &self.entries)
    }

    /// Works out the lines again after the table, the filter, the sort
    /// order or the collapsed groups changed.
    fn rebuild(&mut self) {
        let rows = &self.table.rows;
        let order = self.table.order(self.sort, &self.filter);
        self.matching = order.len();
        let mut entries = Vec::with_capacity(order.len());
        let mut next = 0;
        while let Some(&first) = order.get(next) {
            let Some(group) = &rows[first].group else {
                entries.push(Entry::Row(first));
                next += 1;
                continue;
            };
            // `Table::order` keeps the rows of a group next to each other.
            let members = order[next..]
                .iter()
                .take_while(|&&row| rows[row].group.as_ref() == Some(group))
                .count();
            entries.push(Entry::Group {
                name: group.clone(),
                count: members,
            });
            if !self.collapsed.contains(group) {
                let shown = &order[next..next + members];
                entries.extend(shown.iter().map(|&row| Entry::Row(row)));
            }
            next += members;
        }
        self.entries = entries;
        let shown: HashSet<&str> = selectable(&self.table, &self.entries).into_iter().collect();
        self.selection.retain(|id| shown.contains(id));
        self.generation += 1;
    }
}

fn shown_rows<'a>(table: &'a Table, entries: &'a [Entry]) -> impl Iterator<Item = &'a Row> {
    entries.iter().filter_map(|entry| match entry {
        Entry::Row(row) => Some(&table.rows[*row]),
        Entry::Group { .. } => None,
    })
}

/// The IDs of the rows that show and can be removed, in the order they show.
fn selectable<'a>(table: &'a Table, entries: &'a [Entry]) -> Vec<&'a str> {
    shown_rows(table, entries)
        .filter(|row| row.flags.removable)
        .map(|row| row.id.as_str())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::UNKNOWN;
    use crate::table::testing::{container, network, usage};
    use crate::table::{containers_table, networks_table};

    fn containers() -> ListModel {
        let mut list = ListModel::new(ResourceKind::Container);
        list.set_table(containers_table(
            &[
                container("solo-b", None, false),
                container("web", Some("shop"), true),
                container("solo-a", None, true),
                container("db", Some("shop"), true),
                container("api", Some("alpha"), false),
            ],
            None,
        ));
        list
    }

    /// The lines as text: `[shop 2]` for a group, the name for a row, with
    /// a `*` when it is selected.
    fn lines(list: &ListModel) -> Vec<String> {
        (0..list.len())
            .map(|index| match list.item(index).unwrap() {
                Item::Group {
                    name,
                    count,
                    collapsed,
                } => {
                    let mark = if collapsed { "+" } else { "" };
                    format!("[{name} {count}{mark}]")
                }
                Item::Row(row) if list.is_selected(&row.id) => format!("{}*", row.name),
                Item::Row(row) => row.name.clone(),
            })
            .collect()
    }

    #[test]
    fn groups_come_with_a_header_line() {
        let list = containers();
        assert_eq!(
            lines(&list),
            [
                "[alpha 1]",
                "api",
                "[shop 2]",
                "db",
                "web",
                "solo-a",
                "solo-b"
            ]
        );
        assert!(list.item(7).is_none());
        assert_eq!(list.columns().len(), 8);
        assert_eq!(list.kind(), ResourceKind::Container);
    }

    #[test]
    fn a_collapsed_group_keeps_its_header_and_hides_its_rows() {
        let mut list = containers();
        list.toggle_group("shop");
        assert_eq!(
            lines(&list),
            ["[alpha 1]", "api", "[shop 2+]", "solo-a", "solo-b"]
        );
        list.toggle_group("shop");
        assert_eq!(list.len(), 7);
    }

    #[test]
    fn the_filter_narrows_the_lines_and_the_group_counts() {
        let mut list = containers();
        list.set_filter("running");
        assert_eq!(lines(&list), ["[shop 2]", "db", "web", "solo-a"]);
        list.set_filter("web");
        assert_eq!(lines(&list), ["[shop 1]", "web"]);
        assert_eq!(list.filter(), "web");
        list.set_filter("no such thing");
        assert!(list.is_empty());
        assert_eq!(list.empty_text(), "Nothing matches the filter.");
        list.set_filter("");
        assert_eq!(list.len(), 7);
    }

    #[test]
    fn an_empty_list_says_there_is_nothing() {
        let list = ListModel::new(ResourceKind::Volume);
        assert!(list.is_empty());
        assert_eq!(list.empty_text(), "No volumes.");
    }

    #[test]
    fn a_header_click_sorts_up_then_down_then_not_at_all() {
        let mut list = containers();
        list.toggle_sort(container_column::NAME);
        assert_eq!(
            list.sort().map(|sort| sort.direction),
            Some(SortDirection::Ascending)
        );
        list.toggle_sort(container_column::NAME);
        assert_eq!(
            lines(&list),
            [
                "[alpha 1]",
                "api",
                "[shop 2]",
                "web",
                "db",
                "solo-b",
                "solo-a"
            ]
        );
        list.toggle_sort(container_column::NAME);
        assert_eq!(list.sort(), None);
        // Another column starts ascending again.
        list.toggle_sort(container_column::STATE);
        list.toggle_sort(container_column::IMAGE);
        let sort = list.sort().unwrap();
        assert_eq!(sort.column, container_column::IMAGE);
        assert_eq!(sort.direction, SortDirection::Ascending);
    }

    #[test]
    fn select_all_takes_the_rows_that_show() {
        let mut list = containers();
        list.set_filter("solo");
        list.select_all();
        assert_eq!(lines(&list), ["solo-a*", "solo-b*"]);
        assert_eq!(list.selected_count(), 2);
        assert_eq!(list.coverage(), Coverage::All);
    }

    #[test]
    fn select_all_leaves_out_the_rows_of_a_collapsed_group() {
        let mut list = containers();
        list.toggle_group("shop");
        list.select_all();
        assert_eq!(list.selected_count(), 3);
        list.toggle_group("shop");
        assert_eq!(
            lines(&list),
            [
                "[alpha 1]",
                "api*",
                "[shop 2]",
                "db",
                "web",
                "solo-a*",
                "solo-b*"
            ]
        );
        assert_eq!(list.coverage(), Coverage::Some);
    }

    #[test]
    fn the_select_all_box_selects_what_shows_and_then_nothing() {
        let mut list = containers();
        list.toggle("id-web");
        list.toggle_all();
        assert_eq!(list.selected_count(), 5);
        list.toggle_all();
        assert_eq!(list.selected_count(), 0);
    }

    #[test]
    fn a_groups_box_selects_the_rows_under_its_header() {
        let mut list = containers();
        assert_eq!(list.group_coverage("shop"), Coverage::None);
        list.toggle("id-db");
        assert_eq!(list.group_coverage("shop"), Coverage::Some);
        list.toggle_group_selection("shop");
        assert_eq!(list.group_coverage("shop"), Coverage::All);
        assert_eq!(list.selected_count(), 2);
        list.toggle_group_selection("shop");
        assert_eq!(list.selected_count(), 0);
        // A collapsed group shows no rows, so its box selects none.
        list.toggle_group("shop");
        list.toggle_group_selection("shop");
        assert_eq!(list.selected_count(), 0);
        assert_eq!(list.group_coverage("shop"), Coverage::None);
    }

    #[test]
    fn the_counts_tell_the_filter_from_the_collapsed_groups() {
        let mut list = containers();
        assert_eq!((list.matching(), list.total()), (5, 5));
        list.toggle_group("shop");
        assert_eq!((list.matching(), list.total()), (5, 5));
        list.set_filter("running");
        assert_eq!((list.matching(), list.total()), (3, 5));
    }

    #[test]
    fn rows_that_a_filter_hides_are_deselected() {
        let mut list = containers();
        list.select_all();
        assert_eq!(list.selected_count(), 5);
        list.set_filter("shop");
        assert_eq!(list.selected_count(), 2);
        // They do not come back selected.
        list.set_filter("");
        assert_eq!(list.selected_count(), 2);
        assert!(list.is_selected("id-db") && list.is_selected("id-web"));
    }

    #[test]
    fn the_selection_survives_a_reload_minus_what_vanished() {
        let mut list = containers();
        list.toggle("id-web");
        list.toggle("id-solo-a");
        list.toggle("id-api");
        list.set_table(containers_table(
            &[
                container("web", Some("shop"), false),
                container("api", Some("alpha"), false),
                container("new", None, true),
            ],
            None,
        ));
        assert_eq!(list.selected_count(), 2);
        assert_eq!(
            lines(&list),
            ["[alpha 1]", "api*", "[shop 1]", "web*", "new"]
        );
    }

    #[test]
    fn a_range_follows_the_order_on_screen() {
        let mut list = containers();
        list.toggle("id-db");
        list.select_range_to("id-solo-a");
        assert_eq!(
            lines(&list),
            [
                "[alpha 1]",
                "api",
                "[shop 2]",
                "db*",
                "web*",
                "solo-a*",
                "solo-b"
            ]
        );
        list.clear_selection();
        assert_eq!(list.selected_count(), 0);
        assert_eq!(list.coverage(), Coverage::None);
    }

    #[test]
    fn rows_that_do_not_show_or_cannot_be_removed_cannot_be_selected() {
        let mut list = containers();
        list.set_filter("solo");
        list.toggle("id-web");
        list.select_range_to("id-web");
        list.toggle("no-such-row");
        assert_eq!(list.selected_count(), 0);

        let mut networks = ListModel::new(ResourceKind::Network);
        networks.set_table(networks_table(&[network("bridge"), network("app")]));
        networks.toggle("net-bridge");
        assert_eq!(networks.selected_count(), 0);
        networks.select_all();
        assert_eq!(networks.selected_count(), 1);
        assert!(networks.is_selected("net-app"));
        assert_eq!(networks.coverage(), Coverage::All);
    }

    #[test]
    fn the_removal_plan_lists_the_selection_in_the_order_on_screen() {
        let mut list = containers();
        list.toggle("id-solo-b");
        list.toggle("id-web");
        list.toggle("id-api");
        let plan = list.removal_plan();
        assert_eq!(plan.kind, ResourceKind::Container);
        assert_eq!(plan.ids(), ["id-api", "id-web", "id-solo-b"]);
        assert_eq!(
            plan.confirmation(),
            "Remove 3 containers? 1 is running and will be stopped."
        );
    }

    #[test]
    fn usage_arrives_in_place_and_reorders_a_list_sorted_by_it() {
        let mut list = ListModel::new(ResourceKind::Container);
        list.set_table(containers_table(
            &[container("a", None, true), container("b", None, true)],
            None,
        ));
        let cpu = |list: &ListModel, id: &str| list.row(id).unwrap().cells[4].clone();
        assert_eq!(cpu(&list, "id-a"), UNKNOWN);
        let before = list.generation();
        list.set_usage(Some(&[usage("a", Some(1.0), 1), usage("b", Some(50.0), 1)]));
        assert_eq!(cpu(&list, "id-a"), "1.0%");
        assert!(list.generation() > before);

        list.set_sort(Some(Sort {
            column: container_column::CPU,
            direction: SortDirection::Descending,
        }));
        assert_eq!(lines(&list), ["b", "a"]);
        list.set_usage(Some(&[usage("a", Some(80.0), 1), usage("b", Some(2.0), 1)]));
        assert_eq!(lines(&list), ["a", "b"]);
    }

    #[test]
    fn a_table_of_another_kind_starts_a_new_view() {
        let mut list = containers();
        list.toggle_sort(container_column::DISK_IO);
        list.toggle_group("shop");
        list.select_all();
        list.set_table(networks_table(&[network("app"), network("db")]));
        assert_eq!(list.kind(), ResourceKind::Network);
        assert_eq!(list.sort(), None);
        assert_eq!(list.selected_count(), 0);
        assert_eq!(lines(&list), ["app", "db"]);
    }

    #[test]
    fn busy_rows_are_remembered_until_they_vanish() {
        let mut list = containers();
        list.set_busy("id-web", true);
        assert!(list.is_busy("id-web"));
        assert!(!list.is_busy("id-db"));
        list.set_table(containers_table(&[container("db", None, true)], None));
        assert!(!list.is_busy("id-web"));
        list.set_busy("id-db", true);
        list.set_busy("id-db", false);
        assert!(!list.is_busy("id-db"));
    }

    #[test]
    fn the_generation_changes_with_everything_that_shows() {
        let mut list = containers();
        let mut last = list.generation();
        let mut changed = |list: &ListModel| {
            let now = list.generation();
            let changed = now != last;
            last = now;
            changed
        };
        list.toggle("id-web");
        assert!(changed(&list));
        list.set_filter("shop");
        assert!(changed(&list));
        // The same filter again is no change.
        list.set_filter("shop");
        assert!(!changed(&list));
        list.toggle_group("shop");
        assert!(changed(&list));
        list.select_all();
        assert!(changed(&list));
        list.set_busy("id-web", true);
        assert!(changed(&list));
        list.set_busy("id-web", true);
        assert!(!changed(&list));
    }
}
