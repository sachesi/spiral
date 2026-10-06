//! Where the icons of the desktop go: a grid of cells over the area, filled a column at a
//! time from the top right or left, and the three ways of arranging them on it.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

/// A point or a size in the desktop's logical pixels, from its top left corner.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

/// A cell of the grid: its column and its row.
pub type Cell = (usize, usize);

/// How the icons are laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Arrangement {
    /// On the cells of the grid, each where it was put.
    #[default]
    Grid,
    /// On the cells of the grid in the order of the sort, none out of place.
    Auto,
    /// Anywhere, to the pixel.
    Free,
}

impl Arrangement {
    pub fn from_nick(nick: &str) -> Option<Self> {
        match nick {
            "grid" => Some(Self::Grid),
            "auto" => Some(Self::Auto),
            "free" => Some(Self::Free),
            _ => None,
        }
    }
}

/// The cells over an area: as many whole cells as fit inside its margin, at least one,
/// sharing out what is left over, so the last column and row lie at the margin as the first
/// do. The first column is at the left, or at the right when the grid is aligned there.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Grid {
    area: Point,
    cell: Point,
    margin: f64,
    columns: usize,
    rows: usize,
    right: bool,
}

impl Grid {
    pub fn new(area: Point, cell: Point, margin: f64) -> Self {
        let cell = Point::new(cell.x.max(1.0), cell.y.max(1.0));
        let fit =
            |length: f64, step: f64| ((length - 2.0 * margin) / step).floor().max(1.0) as usize;
        let (columns, rows) = (fit(area.x, cell.x), fit(area.y, cell.y));
        let spread = |length: f64, step: f64, count: usize| {
            ((length - 2.0 * margin) / count as f64).max(step)
        };
        Self {
            area,
            cell: Point::new(
                spread(area.x, cell.x, columns),
                spread(area.y, cell.y, rows),
            ),
            margin,
            columns,
            rows,
            right: false,
        }
    }

    /// The grid with its first column at the right edge when `right` is set.
    pub fn aligned_right(self, right: bool) -> Self {
        Self { right, ..self }
    }

    /// The left edge of a tile at `x` seen from the side the grid starts at, the same
    /// measure from either side, so going there and back gives `x` again.
    pub fn seen_from_start(&self, x: f64) -> f64 {
        if self.right {
            self.area.x - self.cell.x - x
        } else {
            x
        }
    }

    pub fn columns(&self) -> usize {
        self.columns
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn cell_size(&self) -> Point {
        self.cell
    }

    /// The top left corner of `cell`.
    pub fn origin(&self, (column, row): Cell) -> Point {
        Point::new(
            self.seen_from_start(self.margin + column as f64 * self.cell.x),
            self.margin + row as f64 * self.cell.y,
        )
    }

    /// Where a tile at `point` is in cells from the corner the grid starts at, as places are
    /// kept: a tile stays in its cell whatever size the cells have.
    pub fn in_cells(&self, point: Point) -> Point {
        Point::new(
            (self.seen_from_start(point.x) - self.margin) / self.cell.x,
            (point.y - self.margin) / self.cell.y,
        )
    }

    /// The top left corner of a tile `cells` from the corner the grid starts at.
    pub fn at_cells(&self, cells: Point) -> Point {
        Point::new(
            self.seen_from_start(self.margin + cells.x * self.cell.x),
            self.margin + cells.y * self.cell.y,
        )
    }

    /// The cell whose middle is nearest a tile whose top left corner is at `point`.
    pub fn cell_at(&self, point: Point) -> Cell {
        let index = |at: f64, step: f64, count: usize| {
            (((at - self.margin) / step).round().max(0.0) as usize).min(count - 1)
        };
        (
            index(self.seen_from_start(point.x), self.cell.x, self.columns),
            index(point.y, self.cell.y, self.rows),
        )
    }

    /// Every cell, a column at a time from the top of the first.
    pub fn cells(&self) -> impl Iterator<Item = Cell> + '_ {
        (0..self.columns).flat_map(move |column| (0..self.rows).map(move |row| (column, row)))
    }

    /// The free cell nearest `wanted`, the one earliest in column order among those as near.
    pub fn nearest_free(&self, wanted: Cell, taken: &HashSet<Cell>) -> Option<Cell> {
        let distance = |(column, row): Cell| {
            let dx = column as f64 - wanted.0 as f64;
            let dy = row as f64 - wanted.1 as f64;
            dx * dx + dy * dy
        };
        self.cells()
            .filter(|cell| !taken.contains(cell))
            .min_by(|a, b| distance(*a).total_cmp(&distance(*b)))
    }

    /// A tile put at `point`, moved as little as it takes to lie wholly inside the margin.
    pub fn inside(&self, point: Point) -> Point {
        let clamp = |at: f64, length: f64, step: f64| {
            at.clamp(self.margin, (length - self.margin - step).max(self.margin))
        };
        Point::new(
            clamp(point.x, self.area.x, self.cell.x),
            clamp(point.y, self.area.y, self.cell.y),
        )
    }
}

/// Where each of `items`, given in the order of the sort, goes: from `kept`, the places
/// they were left in, as `arrangement` has it. Items with no place take the first free
/// cells; when the grid is full the rest share its last cell.
pub fn arrange<K: Hash + Eq + Clone>(
    grid: &Grid,
    arrangement: Arrangement,
    items: &[K],
    kept: &HashMap<K, Point>,
) -> HashMap<K, Point> {
    let last = (grid.columns() - 1, grid.rows() - 1);
    let mut placed = HashMap::with_capacity(items.len());
    match arrangement {
        Arrangement::Auto => {
            let mut cells = grid.cells();
            for item in items {
                let cell = cells.next().unwrap_or(last);
                placed.insert(item.clone(), grid.origin(cell));
            }
        }
        Arrangement::Grid => {
            let mut taken = HashSet::new();
            for item in items {
                if let Some(&point) = kept.get(item) {
                    let cell = grid
                        .nearest_free(grid.cell_at(point), &taken)
                        .unwrap_or(last);
                    taken.insert(cell);
                    placed.insert(item.clone(), grid.origin(cell));
                }
            }
            fill_free_cells(grid, items, &mut placed, &mut taken);
        }
        Arrangement::Free => {
            let mut taken = HashSet::new();
            for item in items {
                if let Some(&point) = kept.get(item) {
                    let point = grid.inside(point);
                    taken.extend(cells_under(grid, point));
                    placed.insert(item.clone(), point);
                }
            }
            fill_free_cells(grid, items, &mut placed, &mut taken);
        }
    }
    placed
}

/// Moves `moved` by `by` from where they are in `placed`, keeping their places relative to
/// each other, and settles them as `arrangement` has it; the rest stay. Nothing moves when
/// the icons are arranged by the sort.
pub fn move_by<K: Hash + Eq + Clone>(
    grid: &Grid,
    arrangement: Arrangement,
    placed: &HashMap<K, Point>,
    moved: &[K],
    by: Point,
) -> HashMap<K, Point> {
    let mut result = placed.clone();
    if arrangement == Arrangement::Auto {
        return result;
    }
    let shifted = |item: &K| {
        placed
            .get(item)
            .map(|at| grid.inside(Point::new(at.x + by.x, at.y + by.y)))
    };
    if arrangement == Arrangement::Free {
        for item in moved {
            if let Some(point) = shifted(item) {
                result.insert(item.clone(), point);
            }
        }
        return result;
    }
    let moving: HashSet<&K> = moved.iter().collect();
    let mut taken: HashSet<Cell> = placed
        .iter()
        .filter(|(item, _)| !moving.contains(item))
        .map(|(_, &point)| grid.cell_at(point))
        .collect();
    for item in moved {
        let Some(point) = shifted(item) else {
            continue;
        };
        let cell = grid
            .nearest_free(grid.cell_at(point), &taken)
            .unwrap_or_else(|| grid.cell_at(point));
        taken.insert(cell);
        result.insert(item.clone(), grid.origin(cell));
    }
    result
}

/// Places kept for an area `from` large, made to fit one `to` large by scaling them.
pub fn rescale<K: Hash + Eq + Clone>(
    kept: &HashMap<K, Point>,
    from: Point,
    to: Point,
) -> HashMap<K, Point> {
    let (sx, sy) = (to.x / from.x.max(1.0), to.y / from.y.max(1.0));
    kept.iter()
        .map(|(item, point)| (item.clone(), Point::new(point.x * sx, point.y * sy)))
        .collect()
}

/// The cells a tile at `point` covers some of.
fn cells_under(grid: &Grid, point: Point) -> impl Iterator<Item = Cell> {
    let cell = grid.cell_size();
    let (x0, y0) = (grid.seen_from_start(point.x), point.y);
    let (x1, y1) = (x0 + cell.x - 1.0, y0 + cell.y - 1.0);
    let corners = [
        Point::new(x0, y0),
        Point::new(x1, y0),
        Point::new(x0, y1),
        Point::new(x1, y1),
    ];
    let floor = |p: Point| {
        let index = |at: f64, step: f64, count: usize| {
            (((at - grid.margin) / step).floor().max(0.0) as usize).min(count - 1)
        };
        (
            index(p.x, cell.x, grid.columns()),
            index(p.y, cell.y, grid.rows()),
        )
    };
    let set: HashSet<Cell> = corners.into_iter().map(floor).collect();
    set.into_iter()
}

fn fill_free_cells<K: Hash + Eq + Clone>(
    grid: &Grid,
    items: &[K],
    placed: &mut HashMap<K, Point>,
    taken: &mut HashSet<Cell>,
) {
    let last = (grid.columns() - 1, grid.rows() - 1);
    let mut free = grid
        .cells()
        .filter(|cell| !taken.contains(cell))
        .collect::<Vec<_>>()
        .into_iter();
    for item in items {
        if placed.contains_key(item) {
            continue;
        }
        let cell = free.next().unwrap_or(last);
        taken.insert(cell);
        placed.insert(item.clone(), grid.origin(cell));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three columns and two rows of 100 by 100, inside a margin of 10.
    fn grid() -> Grid {
        Grid::new(Point::new(320.0, 220.0), Point::new(100.0, 100.0), 10.0)
    }

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    fn kept(places: &[(&str, f64, f64)]) -> HashMap<String, Point> {
        places
            .iter()
            .map(|&(n, x, y)| (n.to_string(), Point::new(x, y)))
            .collect()
    }

    fn cell_of(grid: &Grid, placed: &HashMap<String, Point>, name: &str) -> Cell {
        grid.cell_at(placed[name])
    }

    #[test]
    fn the_grid_holds_the_whole_cells_that_fit_inside_the_margin() {
        let grid = grid();
        assert_eq!((grid.columns(), grid.rows()), (3, 2));
        assert_eq!(grid.origin((2, 1)), Point::new(210.0, 110.0));
        // Too small for one cell, it still has one.
        let tiny = Grid::new(Point::new(50.0, 50.0), Point::new(100.0, 100.0), 10.0);
        assert_eq!((tiny.columns(), tiny.rows()), (1, 1));
    }

    #[test]
    fn the_cells_share_out_what_is_left_so_the_last_lies_at_the_margin() {
        let grid = Grid::new(Point::new(350.0, 250.0), Point::new(100.0, 100.0), 10.0);
        assert_eq!((grid.columns(), grid.rows()), (3, 2));
        assert_eq!(grid.cell_size(), Point::new(110.0, 115.0));
        assert_eq!(grid.origin((2, 1)), Point::new(230.0, 125.0));
        let right = grid.aligned_right(true);
        assert_eq!(right.origin((0, 0)), Point::new(230.0, 10.0));
        assert_eq!(right.origin((2, 0)), Point::new(10.0, 10.0));
    }

    #[test]
    fn aligned_right_the_first_column_is_at_the_right_edge() {
        let grid = grid().aligned_right(true);
        assert_eq!(grid.origin((0, 0)), Point::new(210.0, 10.0));
        assert_eq!(grid.origin((2, 1)), Point::new(10.0, 110.0));
        assert_eq!(grid.cell_at(Point::new(205.0, 14.0)), (0, 0));
        let items = names(&["a", "b", "c"]);
        let placed = arrange(&grid, Arrangement::Grid, &items, &HashMap::new());
        assert_eq!(placed["a"], Point::new(210.0, 10.0));
        assert_eq!(placed["c"], Point::new(110.0, 10.0));
    }

    #[test]
    fn a_tile_kept_in_cells_stays_in_its_cell_as_the_cells_grow() {
        for right in [false, true] {
            let small = grid().aligned_right(right);
            let large = Grid::new(Point::new(320.0, 220.0), Point::new(150.0, 100.0), 10.0)
                .aligned_right(right);
            let kept = small.in_cells(small.origin((1, 1)));
            assert_eq!(kept, Point::new(1.0, 1.0));
            assert_eq!(large.cell_at(large.at_cells(kept)), (1, 1));
            assert_eq!(large.at_cells(kept), large.origin((1, 1)));
        }
    }

    #[test]
    fn aligned_right_freely_placed_icons_cover_the_cells_under_them() {
        let grid = grid().aligned_right(true);
        let items = names(&["a", "b", "c"]);
        let placed = arrange(
            &grid,
            Arrangement::Free,
            &items,
            &kept(&[("a", 160.0, 10.0)]),
        );
        // "a" lies over the first two columns of the top row, so the others go under it.
        assert_eq!(cell_of(&grid, &placed, "b"), (0, 1));
        assert_eq!(cell_of(&grid, &placed, "c"), (1, 1));
    }

    #[test]
    fn new_icons_fill_a_column_before_the_next() {
        let grid = grid();
        let items = names(&["a", "b", "c", "d"]);
        let placed = arrange(&grid, Arrangement::Grid, &items, &HashMap::new());
        let cells: Vec<Cell> = items.iter().map(|n| cell_of(&grid, &placed, n)).collect();
        assert_eq!(cells, [(0, 0), (0, 1), (1, 0), (1, 1)]);
    }

    #[test]
    fn a_kept_place_holds_and_new_icons_go_around_it() {
        let grid = grid();
        let items = names(&["new", "moved"]);
        let placed = arrange(
            &grid,
            Arrangement::Grid,
            &items,
            &kept(&[("moved", 12.0, 8.0)]),
        );
        assert_eq!(cell_of(&grid, &placed, "moved"), (0, 0));
        assert_eq!(cell_of(&grid, &placed, "new"), (0, 1));
    }

    #[test]
    fn two_kept_in_one_cell_part_to_the_nearest_free_one() {
        let grid = grid();
        let items = names(&["a", "b"]);
        let placed = arrange(
            &grid,
            Arrangement::Grid,
            &items,
            &kept(&[("a", 110.0, 10.0), ("b", 115.0, 12.0)]),
        );
        assert_eq!(cell_of(&grid, &placed, "a"), (1, 0));
        assert_eq!(cell_of(&grid, &placed, "b"), (0, 0));
    }

    #[test]
    fn arranged_by_the_sort_the_kept_places_do_not_count() {
        let grid = grid();
        let items = names(&["b", "a"]);
        let placed = arrange(
            &grid,
            Arrangement::Auto,
            &items,
            &kept(&[("a", 210.0, 110.0)]),
        );
        assert_eq!(cell_of(&grid, &placed, "b"), (0, 0));
        assert_eq!(cell_of(&grid, &placed, "a"), (0, 1));
    }

    #[test]
    fn a_full_grid_piles_the_rest_in_its_last_cell() {
        let grid = grid();
        let items: Vec<String> = (0..8).map(|n| n.to_string()).collect();
        let placed = arrange(&grid, Arrangement::Grid, &items, &HashMap::new());
        assert_eq!(cell_of(&grid, &placed, "6"), (2, 1));
        assert_eq!(cell_of(&grid, &placed, "7"), (2, 1));
    }

    #[test]
    fn freely_placed_icons_stay_to_the_pixel_inside_the_area() {
        let grid = grid();
        let items = names(&["a", "b", "c"]);
        let placed = arrange(
            &grid,
            Arrangement::Free,
            &items,
            &kept(&[("a", 47.0, 13.0), ("b", 900.0, -40.0)]),
        );
        assert_eq!(placed["a"], Point::new(47.0, 13.0));
        assert_eq!(placed["b"], Point::new(210.0, 10.0));
        // The new one takes a cell neither of them covers any of.
        assert_eq!(cell_of(&grid, &placed, "c"), (2, 1));
    }

    #[test]
    fn icons_moved_together_keep_their_places_to_each_other() {
        let grid = grid();
        let items = names(&["a", "b", "c"]);
        let placed = arrange(&grid, Arrangement::Grid, &items, &HashMap::new());
        let moved = move_by(
            &grid,
            Arrangement::Grid,
            &placed,
            &names(&["a", "b"]),
            Point::new(200.0, 0.0),
        );
        assert_eq!(cell_of(&grid, &moved, "a"), (2, 0));
        assert_eq!(cell_of(&grid, &moved, "b"), (2, 1));
        assert_eq!(cell_of(&grid, &moved, "c"), (1, 0));
    }

    #[test]
    fn an_icon_moved_onto_another_takes_the_nearest_free_cell() {
        let grid = grid();
        let items = names(&["a", "b", "c", "d"]);
        let placed = arrange(&grid, Arrangement::Grid, &items, &HashMap::new());
        let moved = move_by(
            &grid,
            Arrangement::Grid,
            &placed,
            &names(&["a"]),
            Point::new(100.0, 100.0),
        );
        assert_eq!(cell_of(&grid, &moved, "d"), (1, 1));
        assert_eq!(cell_of(&grid, &moved, "a"), (2, 1));
    }

    #[test]
    fn nothing_moves_while_the_sort_arranges_the_icons() {
        let grid = grid();
        let items = names(&["a"]);
        let placed = arrange(&grid, Arrangement::Auto, &items, &HashMap::new());
        let moved = move_by(
            &grid,
            Arrangement::Auto,
            &placed,
            &items,
            Point::new(100.0, 100.0),
        );
        assert_eq!(moved, placed);
    }

    #[test]
    fn a_free_move_goes_to_the_pixel_and_stops_at_the_edge() {
        let grid = grid();
        let items = names(&["a"]);
        let placed = arrange(
            &grid,
            Arrangement::Free,
            &items,
            &kept(&[("a", 20.0, 20.0)]),
        );
        let moved = move_by(
            &grid,
            Arrangement::Free,
            &placed,
            &items,
            Point::new(33.0, 500.0),
        );
        assert_eq!(moved["a"], Point::new(53.0, 110.0));
    }

    #[test]
    fn places_kept_for_another_size_are_scaled_to_this_one() {
        let from = Point::new(1000.0, 500.0);
        let to = Point::new(2000.0, 1000.0);
        let scaled = rescale(&kept(&[("a", 100.0, 50.0)]), from, to);
        assert_eq!(scaled["a"], Point::new(200.0, 100.0));
    }

    #[test]
    fn arrangements_are_kept_by_their_nicks() {
        for (nick, arrangement) in [
            ("grid", Arrangement::Grid),
            ("auto", Arrangement::Auto),
            ("free", Arrangement::Free),
        ] {
            assert_eq!(Arrangement::from_nick(nick), Some(arrangement));
        }
        assert_eq!(Arrangement::from_nick("spiral"), None);
    }
}
