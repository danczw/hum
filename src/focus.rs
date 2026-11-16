/// Names a row across ticks so the cursor follows it when rows reorder:
/// `(pid, start time)` for processes, `(port, pid)` for ports.
pub type Key = (u64, u64);

/// Sections in the order `Tab` cycles through.
pub const AGENTS: usize = 0;
pub const ORPHANS: usize = 1;
pub const PORTS: usize = 2;
const SECTIONS: usize = 3;

/// One section's cursor and scroll position.
#[derive(Default)]
pub struct Pane {
    /// The rows of the last frame, for keys pressed before the next one.
    keys: Vec<Key>,
    index: usize,
    /// The selected row; when it is gone the cursor stays at `index`, on the
    /// row that took its place.
    key: Option<Key>,
    offset: usize,
    /// Data rows the last frame showed.
    page: usize,
}

/// The rows of a section one frame shows.
#[derive(Debug, PartialEq)]
pub struct Window {
    pub offset: usize,
    /// How many rows are shown, from `offset`.
    pub rows: usize,
    pub selected: usize,
    /// Rows hidden below, counted on a `+N more` line after the shown rows.
    pub more: usize,
}

impl Pane {
    /// Finds the selected row among `keys` and scrolls so it shows in
    /// `height` lines. When rows remain below and at least two lines fit, a
    /// `+N more` line takes the last one.
    pub fn frame(&mut self, keys: Vec<Key>, height: usize) -> Window {
        let len = keys.len();
        if let Some(i) = self.key.and_then(|k| keys.iter().position(|&x| x == k)) {
            self.index = i;
        }
        self.index = self.index.min(len.saturating_sub(1));
        self.key = keys.get(self.index).copied();
        self.keys = keys;

        let (offset, rows, more) = if len <= height {
            (0, len, 0)
        } else if height < 2 {
            // no line to spare for `+N more`
            (self.index, height, 0)
        } else {
            // at the end the last line goes to a row, not to `+0 more`
            let last = len - height;
            let mut offset = self.offset.min(last).min(self.index);
            if self.index + 2 > offset + height {
                offset = (self.index + 2 - height).min(last);
            }
            let rows = if offset < last { height - 1 } else { height };
            (offset, rows, len - offset - rows)
        };
        self.offset = offset;
        self.page = rows.max(1);
        Window {
            offset,
            rows,
            selected: self.index,
            more,
        }
    }

    fn select(&mut self, index: usize) {
        if let Some(last) = self.keys.len().checked_sub(1) {
            self.index = index.min(last);
            self.key = Some(self.keys[self.index]);
        }
    }
}

/// Which section has the cursor, and whether it shows at all.
#[derive(Default)]
pub struct Focus {
    panes: [Pane; SECTIONS],
    focused: usize,
    /// Starts off so the default view stays a glance.
    active: bool,
}

impl Focus {
    pub fn pane(&mut self, section: usize) -> &mut Pane {
        &mut self.panes[section]
    }

    /// Whether `section` draws its cursor and an accent title.
    pub fn shows(&self, section: usize) -> bool {
        self.active && self.focused == section
    }

    /// Moves the cursor `by` rows. The first move only shows the cursor.
    pub fn step(&mut self, by: isize) {
        if std::mem::replace(&mut self.active, true) {
            self.move_by(by);
        }
    }

    /// Moves the cursor a page of the rows last shown.
    pub fn page(&mut self, down: bool) {
        self.active = true;
        let page = self.panes[self.focused].page as isize;
        self.move_by(if down { page } else { -page });
    }

    pub fn first(&mut self) {
        self.active = true;
        self.panes[self.focused].select(0);
    }

    pub fn last(&mut self) {
        self.active = true;
        self.panes[self.focused].select(usize::MAX);
    }

    /// Moves focus `by` sections, wrapping and passing over sections
    /// without rows, and shows the cursor.
    pub fn cycle(&mut self, by: isize) {
        self.active = true;
        let mut next = self.focused;
        for _ in 1..SECTIONS {
            next = (next as isize + by).rem_euclid(SECTIONS as isize) as usize;
            if !self.panes[next].keys.is_empty() {
                self.focused = next;
                return;
            }
        }
    }

    /// Hides the cursor and scrolls back to the top. False when the cursor
    /// was not showing.
    pub fn clear(&mut self) -> bool {
        let was = self.active;
        *self = Self::default();
        was
    }

    fn move_by(&mut self, by: isize) {
        let pane = &mut self.panes[self.focused];
        pane.select(pane.index.saturating_add_signed(by));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(ids: impl IntoIterator<Item = u64>) -> Vec<Key> {
        ids.into_iter().map(|id| (id, 0)).collect()
    }

    fn agents(focus: &mut Focus, ids: impl IntoIterator<Item = u64>, height: usize) -> Window {
        focus.pane(0).frame(keys(ids), height)
    }

    #[test]
    fn first_move_only_shows_the_cursor() {
        let mut focus = Focus::default();
        agents(&mut focus, 1..=5, 5);
        assert!(!focus.shows(0));
        focus.step(1);
        assert!(focus.shows(0));
        assert_eq!(agents(&mut focus, 1..=5, 5).selected, 0);
        focus.step(1);
        assert_eq!(agents(&mut focus, 1..=5, 5).selected, 1);
    }

    #[test]
    fn cursor_follows_its_row_when_rows_reorder() {
        let mut focus = Focus::default();
        agents(&mut focus, [1, 2, 3], 5);
        focus.step(1);
        focus.step(1);
        assert_eq!(agents(&mut focus, [3, 1, 2], 5).selected, 2);
    }

    #[test]
    fn exited_row_hands_the_cursor_to_the_next() {
        let mut focus = Focus::default();
        agents(&mut focus, [1, 2, 3], 5);
        focus.step(1);
        focus.step(1);
        assert_eq!(agents(&mut focus, [1, 3], 5).selected, 1);
        // the last row exits: the one before it takes over
        assert_eq!(agents(&mut focus, [1], 5).selected, 0);
    }

    #[test]
    fn window_scrolls_to_keep_the_cursor_and_counts_the_rest() {
        let mut focus = Focus::default();
        let w = agents(&mut focus, 0..10, 4);
        assert_eq!(
            w,
            Window {
                offset: 0,
                rows: 3,
                selected: 0,
                more: 7
            }
        );
        focus.step(1);
        for _ in 0..3 {
            focus.step(1);
        }
        let w = agents(&mut focus, 0..10, 4);
        assert_eq!((w.offset, w.rows, w.more), (1, 3, 6));
        focus.last();
        let w = agents(&mut focus, 0..10, 4);
        // at the end every line holds a row
        assert_eq!((w.offset, w.rows, w.selected, w.more), (6, 4, 9, 0));
        focus.step(-4);
        let w = agents(&mut focus, 0..10, 4);
        assert_eq!((w.offset, w.selected), (5, 5));
    }

    #[test]
    fn one_line_shows_the_cursor_row_without_more() {
        let mut focus = Focus::default();
        agents(&mut focus, 0..10, 1);
        focus.last();
        let w = agents(&mut focus, 0..10, 1);
        assert_eq!((w.offset, w.rows, w.more), (9, 1, 0));
        assert_eq!(agents(&mut focus, 0..10, 0).more, 0);
    }

    #[test]
    fn page_moves_by_the_rows_shown() {
        let mut focus = Focus::default();
        agents(&mut focus, 0..10, 4);
        focus.page(true);
        assert_eq!(agents(&mut focus, 0..10, 4).selected, 3);
        focus.page(false);
        assert_eq!(agents(&mut focus, 0..10, 4).selected, 0);
    }

    #[test]
    fn tab_cycles_sections_and_leaves_their_cursors() {
        let mut focus = Focus::default();
        agents(&mut focus, 0..10, 4);
        focus.pane(1).frame(keys(0..3), 4);
        focus.cycle(1);
        assert!(focus.shows(1));
        focus.step(1);
        assert_eq!(focus.pane(1).frame(keys(0..3), 4).selected, 1);
        assert_eq!(agents(&mut focus, 0..10, 4).selected, 0);
        focus.pane(2).frame(keys(0..1), 4);
        focus.cycle(-2);
        assert!(focus.shows(2));
    }

    #[test]
    fn tab_passes_over_sections_without_rows() {
        let mut focus = Focus::default();
        agents(&mut focus, 0..3, 4);
        focus.pane(1).frame(Vec::new(), 4);
        focus.pane(2).frame(keys(0..1), 4);
        focus.cycle(1);
        assert!(focus.shows(2));
        focus.cycle(1);
        assert!(focus.shows(0));
        // nowhere to go: focus stays
        agents(&mut focus, [], 4);
        focus.pane(2).frame(Vec::new(), 4);
        focus.cycle(1);
        assert!(focus.shows(0));
    }

    #[test]
    fn clear_hides_the_cursor_then_reports_nothing_to_clear() {
        let mut focus = Focus::default();
        assert!(!focus.clear());
        focus.last();
        agents(&mut focus, 0..10, 4);
        assert!(focus.clear());
        assert!(!focus.shows(0));
        assert_eq!(agents(&mut focus, 0..10, 4).offset, 0);
    }
}
