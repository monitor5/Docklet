//! Column widths and the buttons of the actions column, as arithmetic.
//!
//! [`fit`] is the whole layout of a table: which columns show and how wide,
//! and which actions have a button. Whatever the width of the list, what it
//! returns is never wider, so a table is never scrolled sideways.

/// A rectangle in pixels, right and bottom exclusive, like a Win32 `RECT`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn contains(&self, x: i32, y: i32) -> bool {
        (self.left..self.right).contains(&x) && (self.top..self.bottom).contains(&y)
    }
}

/// Splits `available` pixels between columns by weight, giving none less
/// than its minimum. The widths add up to `available` exactly unless the
/// minimums alone are wider, in which case each column gets its minimum.
pub fn distribute(available: i32, columns: &[(u16, i32)]) -> Vec<i32> {
    let mut widths: Vec<i32> = columns.iter().map(|&(_, min)| min.max(0)).collect();
    let mut flexible: Vec<bool> = columns.iter().map(|&(weight, _)| weight > 0).collect();
    // Each pass pins at least one column to its minimum or finishes, so the
    // loop ends within one pass per column.
    for _ in 0..=columns.len() {
        let pinned: i32 = (0..columns.len())
            .filter(|&index| !flexible[index])
            .map(|index| widths[index])
            .sum();
        let weight: i64 = (0..columns.len())
            .filter(|&index| flexible[index])
            .map(|index| i64::from(columns[index].0))
            .sum();
        let pool = available - pinned;
        if weight == 0 || pool <= 0 {
            break;
        }
        let mut squeezed = false;
        for index in 0..columns.len() {
            if !flexible[index] {
                continue;
            }
            let share = (i64::from(pool) * i64::from(columns[index].0) / weight) as i32;
            if share < columns[index].1 {
                widths[index] = columns[index].1.max(0);
                flexible[index] = false;
                squeezed = true;
            } else {
                widths[index] = share;
            }
        }
        if !squeezed {
            // Rounding leaves a few pixels; the last flexible column takes them.
            let used: i32 = widths.iter().sum();
            if let Some(last) = (0..columns.len()).rev().find(|&index| flexible[index]) {
                widths[last] += available - used;
            }
            break;
        }
    }
    widths
}

/// A column as the layout sees it, in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColumnSpec {
    pub weight: u16,
    pub min: i32,
    /// See `Column::priority`: the highest goes first, 0 never.
    pub priority: u8,
}

/// One place for a button in the actions column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ButtonSpec {
    /// Wide enough for the longest label that can stand there.
    pub width: i32,
    /// See `Action::fold`: the highest folds first, 0 last.
    pub fold: u8,
}

/// The sizes around the buttons, in pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ButtonMetrics {
    /// Space between two buttons, and before the first and after the last.
    pub gap: i32,
    /// Space above and below each button inside the row.
    pub inset: i32,
    /// Width of the button that opens the row's menu, which stands for the
    /// buttons that are folded.
    pub more: i32,
}

/// What a button of the actions column is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pressed {
    /// The button in this place (an index into the table's button places).
    Slot(usize),
    /// The button that opens the row's menu.
    More,
}

/// The buttons of the actions column as they show at some width, side by
/// side from the left of the cell.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ButtonRow {
    /// The places that have a button, in order, each with its width.
    pub shown: Vec<(usize, i32)>,
    /// Some buttons are folded: the row ends with the menu button.
    pub more: bool,
    pub metrics: ButtonMetrics,
}

impl ButtonRow {
    fn widths(&self) -> impl Iterator<Item = (Pressed, i32)> + '_ {
        let buttons = self
            .shown
            .iter()
            .map(|&(slot, width)| (Pressed::Slot(slot), width));
        buttons.chain(self.more.then_some((Pressed::More, self.metrics.more)))
    }

    /// Width of the column that holds the buttons; 0 without any.
    pub fn column_width(&self) -> i32 {
        let gap = self.metrics.gap;
        match self.widths().map(|(_, width)| width + gap).sum::<i32>() {
            0 => 0,
            buttons => gap + buttons,
        }
    }

    /// Every button of a row whose actions cell is `cell`, from the left.
    pub fn buttons(&self, cell: Rect) -> impl Iterator<Item = (Pressed, Rect)> + '_ {
        let gap = self.metrics.gap;
        let mut left = cell.left + gap;
        self.widths().map(move |(button, width)| {
            let rect = Rect {
                left,
                top: cell.top + self.metrics.inset,
                right: left + width,
                bottom: cell.bottom - self.metrics.inset,
            };
            left += width + gap;
            (button, rect)
        })
    }

    /// The button under `(x, y)`, if any. Gaps between buttons hit nothing.
    pub fn hit(&self, cell: Rect, x: i32, y: i32) -> Option<(Pressed, Rect)> {
        self.buttons(cell).find(|(_, rect)| rect.contains(x, y))
    }
}

/// A table laid out for one width.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fit {
    /// Width of each column, 0 for one that does not show.
    pub widths: Vec<i32>,
    pub buttons: ButtonRow,
}

impl Fit {
    /// Everything side by side: never more than the width it was made for.
    pub fn total(&self) -> i32 {
        self.widths.iter().sum::<i32>() + self.buttons.column_width()
    }
}

/// Lays a table out in `available` pixels.
///
/// Every column gets at least its minimum and every action its button, if
/// there is room. If there is not, columns go and buttons fold into the
/// row's menu one at a time, the highest `priority` or `fold` first (a
/// column before a button of the same number, a later one before an earlier
/// one), until the rest fits. What is left shares the width by weight.
///
/// Should even the columns and buttons that never give way be too wide
/// (a list a few characters wide), the last buttons fold too and the
/// columns are squeezed below their minimum: narrow, but still not scrolled.
pub fn fit(
    available: i32,
    columns: &[ColumnSpec],
    buttons: &[ButtonSpec],
    metrics: ButtonMetrics,
) -> Fit {
    let available = available.max(0);
    let mut shown = vec![true; columns.len()];
    let mut inline = vec![true; buttons.len()];
    let row = |inline: &[bool]| ButtonRow {
        shown: (0..buttons.len())
            .filter(|&slot| inline[slot])
            .map(|slot| (slot, buttons[slot].width.max(0)))
            .collect(),
        more: inline.contains(&false),
        metrics,
    };
    let minimums = |shown: &[bool]| -> i32 {
        let widths = (0..columns.len()).filter(|&index| shown[index]);
        widths.map(|index| columns[index].min.max(0)).sum()
    };
    // Each pass takes one column or one button away, so there are at most
    // as many passes as there are of both.
    for _ in 0..columns.len() + buttons.len() {
        if minimums(&shown) + row(&inline).column_width() <= available {
            break;
        }
        let column = (0..columns.len())
            .filter(|&index| shown[index] && columns[index].priority > 0)
            .max_by_key(|&index| (columns[index].priority, index));
        let button = (0..buttons.len())
            .filter(|&slot| inline[slot] && buttons[slot].fold > 0)
            .max_by_key(|&slot| (buttons[slot].fold, slot));
        match (column, button) {
            (Some(column), Some(button)) if buttons[button].fold > columns[column].priority => {
                inline[button] = false;
            }
            (Some(column), _) => shown[column] = false,
            (None, Some(button)) => inline[button] = false,
            // Only what never gives way is left. The buttons do after all,
            // the last one first: the menu button stands for them.
            (None, None) => match inline.iter().rposition(|&inline| inline) {
                Some(button) => inline[button] = false,
                None => break,
            },
        }
    }
    let buttons = row(&inline);
    let pool = (available - buttons.column_width()).max(0);
    let specs: Vec<(u16, i32)> = (0..columns.len())
        .filter(|&index| shown[index])
        .map(|index| (columns[index].weight, columns[index].min.max(0)))
        .collect();
    let needed: i32 = specs.iter().map(|&(_, min)| min).sum();
    let mut fitted = if needed > pool {
        // In proportion to the minimums; the first column takes what the
        // rounding leaves.
        let mut widths: Vec<i32> = specs
            .iter()
            .map(|&(_, min)| (i64::from(min) * i64::from(pool) / i64::from(needed)) as i32)
            .collect();
        let used: i32 = widths.iter().sum();
        if let Some(first) = widths.first_mut() {
            *first += pool - used;
        }
        widths
    } else {
        distribute(pool, &specs)
    }
    .into_iter();
    let widths = shown
        .iter()
        .map(|&shown| if shown { fitted.next().unwrap_or(0) } else { 0 })
        .collect();
    Fit { widths, buttons }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_follow_weights_and_fill_the_space() {
        let widths = distribute(600, &[(2, 50), (1, 50), (3, 50)]);
        assert_eq!(widths, vec![200, 100, 300]);
        let widths = distribute(601, &[(1, 10), (1, 10), (1, 10)]);
        assert_eq!(widths.iter().sum::<i32>(), 601);
    }

    #[test]
    fn a_column_below_its_minimum_is_pinned_and_the_rest_share_what_is_left() {
        let widths = distribute(400, &[(1, 200), (1, 50), (2, 50)]);
        // 400 by weight gives the first only 100; it keeps 200, and the
        // other two split 200 as 1:2.
        assert_eq!(widths[0], 200);
        assert_eq!(widths[1] + widths[2], 200);
        assert_eq!(widths[1], 66);
    }

    #[test]
    fn too_little_space_gives_every_column_its_minimum() {
        assert_eq!(distribute(50, &[(1, 40), (1, 40)]), vec![40, 40]);
        assert_eq!(distribute(-10, &[(1, 40)]), vec![40]);
    }

    #[test]
    fn fixed_columns_keep_their_width() {
        let widths = distribute(500, &[(0, 120), (1, 10), (1, 10)]);
        assert_eq!(widths, vec![120, 190, 190]);
        assert_eq!(distribute(500, &[(0, 120)]), vec![120]);
        assert!(distribute(500, &[]).is_empty());
    }

    const METRICS: ButtonMetrics = ButtonMetrics {
        gap: 6,
        inset: 3,
        more: 28,
    };

    fn row(shown: &[(usize, i32)], more: bool) -> ButtonRow {
        ButtonRow {
            shown: shown.to_vec(),
            more,
            metrics: METRICS,
        }
    }

    #[test]
    fn buttons_sit_side_by_side_inside_the_cell_each_as_wide_as_its_label() {
        let buttons = row(&[(0, 40), (2, 60)], true);
        assert_eq!(buttons.column_width(), 6 + 46 + 66 + 34);
        let cell = Rect {
            left: 100,
            top: 20,
            right: 400,
            bottom: 48,
        };
        let rects: Vec<(Pressed, Rect)> = buttons.buttons(cell).collect();
        let at = |left: i32, right: i32| Rect {
            left,
            top: 23,
            right,
            bottom: 45,
        };
        assert_eq!(
            rects,
            [
                (Pressed::Slot(0), at(106, 146)),
                (Pressed::Slot(2), at(152, 212)),
                (Pressed::More, at(218, 246)),
            ]
        );
        // Without buttons there is no column at all.
        assert_eq!(row(&[], false).column_width(), 0);
        assert_eq!(row(&[], true).column_width(), 6 + 34);
    }

    #[test]
    fn hits_land_on_buttons_and_miss_the_gaps() {
        let buttons = row(&[(0, 60), (1, 60)], true);
        let cell = Rect {
            left: 0,
            top: 0,
            right: 300,
            bottom: 28,
        };
        let hit = |x: i32, y: i32| buttons.hit(cell, x, y).map(|(button, _)| button);
        assert_eq!(hit(6, 10), Some(Pressed::Slot(0)));
        assert_eq!(hit(65, 10), Some(Pressed::Slot(0)));
        assert_eq!(hit(66, 10), None);
        assert_eq!(hit(72, 10), Some(Pressed::Slot(1)));
        assert_eq!(hit(140, 10), Some(Pressed::More));
        assert_eq!(hit(200, 10), None);
        assert_eq!(hit(10, 1), None);
        assert_eq!(row(&[], false).hit(cell, 10, 10), None);
    }

    fn column(weight: u16, min: i32, priority: u8) -> ColumnSpec {
        ColumnSpec {
            weight,
            min,
            priority,
        }
    }

    fn button(width: i32, fold: u8) -> ButtonSpec {
        ButtonSpec { width, fold }
    }

    #[test]
    fn with_room_everything_shows_and_the_columns_fill_the_width() {
        let columns = [column(2, 100, 0), column(1, 50, 2), column(1, 50, 1)];
        let buttons = [button(40, 0), button(60, 3)];
        let fit = fit(600, &columns, &buttons, METRICS);
        assert_eq!(fit.buttons, row(&[(0, 40), (1, 60)], false));
        // 600 less the 118 of the buttons, shared 2:1:1.
        assert_eq!(fit.widths, vec![241, 120, 121]);
        assert_eq!(fit.total(), 600);
    }

    #[test]
    fn columns_and_buttons_give_way_in_the_order_of_their_numbers() {
        let columns = [column(2, 100, 0), column(1, 50, 4), column(1, 50, 1)];
        let buttons = [button(40, 0), button(60, 3)];
        let shown = |available: i32| {
            let fit = fit(available, &columns, &buttons, METRICS);
            assert!(fit.total() <= available, "{available}: {fit:?}");
            let columns: Vec<bool> = fit.widths.iter().map(|&width| width > 0).collect();
            let buttons: Vec<usize> = fit.buttons.shown.iter().map(|&(slot, _)| slot).collect();
            (columns, buttons, fit.buttons.more)
        };
        // Everything needs 200 + 118.
        assert_eq!(shown(318), (vec![true, true, true], vec![0, 1], false));
        // One pixel less: the column numbered 4 goes first.
        assert_eq!(shown(317), (vec![true, false, true], vec![0, 1], false));
        // Then the button numbered 3 folds, and the menu button appears.
        assert_eq!(shown(267), (vec![true, false, true], vec![0], true));
        // Then the column numbered 1.
        assert_eq!(shown(235), (vec![true, false, false], vec![0], true));
        // What is numbered 0 stays for as long as it fits at all.
        assert_eq!(shown(186), (vec![true, false, false], vec![0], true));
        assert_eq!(shown(185), (vec![true, false, false], vec![], true));
    }

    #[test]
    fn a_column_goes_before_a_button_of_the_same_number_and_later_ones_first() {
        let columns = [column(1, 100, 2), column(1, 100, 2)];
        let buttons = [button(50, 2)];
        let fit_at = |available: i32| fit(available, &columns, &buttons, METRICS);
        assert_eq!(fit_at(261).widths, vec![199, 0]);
        assert!(!fit_at(261).buttons.more);
        assert_eq!(fit_at(161).widths, vec![0, 0]);
        // Nothing left to show beside it: the button stays while it fits.
        assert!(!fit_at(161).buttons.more);
        assert!(fit_at(61).buttons.more);
    }

    #[test]
    fn a_list_narrower_than_what_never_gives_way_is_squeezed_not_scrolled() {
        let columns = [column(1, 160, 0), column(1, 80, 0)];
        let buttons = [button(70, 0)];
        // 240 for the columns and 82 for the button do not fit in 200: the
        // button folds, and the columns share the rest 2:1.
        let fit = fit(200, &columns, &buttons, METRICS);
        assert_eq!(fit.buttons, row(&[], true));
        assert_eq!(fit.widths, vec![107, 53]);
        assert_eq!(fit.total(), 200);
        for available in 0..400 {
            let fit = super::fit(available, &columns, &buttons, METRICS);
            // Below the width of the menu button alone nothing can fit.
            assert!(fit.total() <= available.max(40), "{available}: {fit:?}");
            assert!(fit.widths.iter().all(|&width| width >= 0));
        }
    }

    #[test]
    fn a_table_without_buttons_or_without_columns_is_laid_out_too() {
        let columns = [column(1, 50, 0), column(1, 50, 1)];
        let plain = fit(300, &columns, &[], METRICS);
        assert_eq!(plain.widths, vec![150, 150]);
        assert_eq!(plain.buttons.column_width(), 0);
        let none = fit(300, &[], &[button(50, 0)], METRICS);
        assert!(none.widths.is_empty());
        assert_eq!(none.total(), 62);
        assert_eq!(fit(-5, &columns, &[], METRICS).total(), 0);
    }
}
