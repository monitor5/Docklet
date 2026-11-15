//! Column widths and the buttons of the actions column, as arithmetic.

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

/// The buttons of the actions column: all the same width, side by side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ButtonRow {
    pub width: i32,
    pub gap: i32,
    /// Space above and below each button inside the row.
    pub inset: i32,
}

impl ButtonRow {
    /// Width of a column that holds `count` buttons.
    pub fn column_width(&self, count: usize) -> i32 {
        self.gap + count as i32 * (self.width + self.gap)
    }

    /// Button `index` of a row whose actions cell is `cell`.
    pub fn button(&self, cell: Rect, index: usize) -> Rect {
        let left = cell.left + self.gap + index as i32 * (self.width + self.gap);
        Rect {
            left,
            top: cell.top + self.inset,
            right: left + self.width,
            bottom: cell.bottom - self.inset,
        }
    }

    /// The button under `(x, y)`, if any. Gaps between buttons hit nothing.
    pub fn hit(&self, cell: Rect, count: usize, x: i32, y: i32) -> Option<usize> {
        (0..count).find(|&index| self.button(cell, index).contains(x, y))
    }
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

    #[test]
    fn buttons_sit_side_by_side_inside_the_cell() {
        let row = ButtonRow {
            width: 60,
            gap: 6,
            inset: 3,
        };
        assert_eq!(row.column_width(3), 6 + 3 * 66);
        let cell = Rect {
            left: 100,
            top: 20,
            right: 400,
            bottom: 48,
        };
        assert_eq!(
            row.button(cell, 1),
            Rect {
                left: 172,
                top: 23,
                right: 232,
                bottom: 45
            }
        );
    }

    #[test]
    fn hits_land_on_buttons_and_miss_the_gaps() {
        let row = ButtonRow {
            width: 60,
            gap: 6,
            inset: 3,
        };
        let cell = Rect {
            left: 0,
            top: 0,
            right: 300,
            bottom: 28,
        };
        assert_eq!(row.hit(cell, 3, 6, 10), Some(0));
        assert_eq!(row.hit(cell, 3, 65, 10), Some(0));
        assert_eq!(row.hit(cell, 3, 66, 10), None);
        assert_eq!(row.hit(cell, 3, 72, 10), Some(1));
        assert_eq!(row.hit(cell, 3, 200, 10), None);
        assert_eq!(row.hit(cell, 3, 10, 1), None);
        assert_eq!(row.hit(cell, 0, 10, 10), None);
    }
}
