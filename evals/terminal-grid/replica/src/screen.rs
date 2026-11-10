//! The replica's screen: a fixed character grid of protected text and input
//! fields, driven the way a host terminal is. Tab moves between input fields,
//! typing overwrites, a full field skips to the next one, and Enter is a round
//! trip to the host during which the keyboard is locked.

use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::time::{Duration, Instant};

#[derive(Debug, Deserialize)]
pub struct Layout {
    pub title: String,
    #[serde(default = "default_rows")]
    pub rows: usize,
    #[serde(default = "default_cols")]
    pub cols: usize,
    #[serde(default)]
    pub font: Option<String>,
    #[serde(default)]
    pub font_px: Option<i32>,
    #[serde(default)]
    pub text: Vec<Literal>,
    #[serde(default)]
    pub fields: Vec<FieldDef>,
    #[serde(default)]
    pub tables: Vec<TableDef>,
    #[serde(default)]
    pub keys: Keys,
    pub message_row: usize,
    #[serde(default)]
    pub message: String,
}

fn default_rows() -> usize {
    27
}
fn default_cols() -> usize {
    80
}

/// Protected text. `blue` draws it the way the host highlights values.
#[derive(Debug, Deserialize)]
pub struct Literal {
    pub row: usize,
    pub col: usize,
    pub text: String,
    #[serde(default)]
    pub blue: bool,
}

#[derive(Debug, Deserialize)]
pub struct FieldDef {
    pub name: String,
    pub row: usize,
    pub col: usize,
    pub len: usize,
    #[serde(default)]
    pub value: String,
}

/// A header line with `visible_rows` lines of input fields under it. `rows`
/// may hold more lines than are visible; the paging keys scroll through them.
#[derive(Debug, Deserialize)]
pub struct TableDef {
    pub name: String,
    pub header_row: usize,
    pub visible_rows: usize,
    pub columns: Vec<ColumnDef>,
    #[serde(default)]
    pub rows: Vec<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct ColumnDef {
    pub name: String,
    pub col: usize,
    pub len: usize,
}

/// What the keys do. Emulators differ here, so every choice is configurable
/// and an eval can cover each variant.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Keys {
    pub enter: EnterMode,
    pub enter_delay_ms: u64,
    /// The chord that erases to the end of the field, e.g. `Ctrl+End`.
    pub erase_eof: String,
    /// PF keys that page a table; PF13-PF24 arrive as Shift+F1-F12.
    pub page_down: u8,
    pub page_up: u8,
}

impl Default for Keys {
    fn default() -> Self {
        Self {
            enter: EnterMode::AddRow,
            enter_delay_ms: 400,
            erase_eof: "Ctrl+End".into(),
            page_down: 8,
            page_up: 7,
        }
    }
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum EnterMode {
    /// The host accepts the screen and opens an empty line in the table the
    /// cursor is in.
    #[default]
    AddRow,
    /// The host accepts the screen and only answers on the message line.
    Submit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Tab,
    BackTab,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    EraseEof,
    Backspace,
    Delete,
    Enter,
    Pf(u8),
}

#[derive(Debug, Clone, Copy)]
enum Bind {
    Field(usize),
    Cell {
        table: usize,
        line: usize,
        column: usize,
    },
}

#[derive(Debug, Clone, Copy)]
struct Slot {
    row: usize,
    col: usize,
    len: usize,
    bind: Bind,
}

struct Table {
    rows: Vec<Vec<String>>,
    offset: usize,
}

pub struct Screen {
    pub layout: Layout,
    fields: Vec<String>,
    tables: Vec<Table>,
    slots: Vec<Slot>,
    pub cursor: (usize, usize),
    pub message: String,
    busy_until: Option<Instant>,
}

impl Screen {
    pub fn new(mut layout: Layout) -> Result<Self, String> {
        let mut slots = Vec::new();
        for (i, f) in layout.fields.iter().enumerate() {
            slots.push(Slot {
                row: f.row,
                col: f.col,
                len: f.len,
                bind: Bind::Field(i),
            });
        }
        for (t, table) in layout.tables.iter().enumerate() {
            for line in 0..table.visible_rows {
                for (column, c) in table.columns.iter().enumerate() {
                    let row = table.header_row + 1 + line;
                    let bind = Bind::Cell {
                        table: t,
                        line,
                        column,
                    };
                    slots.push(Slot {
                        row,
                        col: c.col,
                        len: c.len,
                        bind,
                    });
                }
            }
        }
        for s in &slots {
            if s.len == 0 || s.row >= layout.rows || s.col + s.len > layout.cols {
                return Err(format!(
                    "a field at {},{} does not fit the screen",
                    s.row, s.col
                ));
            }
        }
        slots.sort_by_key(|s| (s.row, s.col));
        let first = slots.first().ok_or("the layout has no input fields")?;
        let cursor = (first.row, first.col);
        let fields = layout.fields.iter().map(|f| f.value.clone()).collect();
        let tables = layout
            .tables
            .iter_mut()
            .map(|t| Table {
                rows: std::mem::take(&mut t.rows),
                offset: 0,
            })
            .collect();
        let message = layout.message.clone();
        Ok(Self {
            layout,
            fields,
            tables,
            slots,
            cursor,
            message,
            busy_until: None,
        })
    }

    pub fn busy(&self, now: Instant) -> bool {
        self.busy_until.is_some_and(|until| now < until)
    }

    /// Handle one key. Keys pressed while the host is busy are dropped, as a
    /// locked terminal keyboard drops them.
    pub fn press(&mut self, key: Key, now: Instant) {
        if self.busy(now) {
            return;
        }
        let slot = self.slot_at(self.cursor);
        match key {
            Key::Char(ch) => match slot {
                Some(i) => self.overwrite(i, ch),
                None => self.message = "Eingabe hier nicht möglich".into(),
            },
            Key::Tab => self.cursor = self.next_start(),
            Key::BackTab => self.cursor = self.previous_start(slot),
            Key::Left => self.step(-1, 0),
            Key::Right => self.step(1, 0),
            Key::Up => self.step(0, -1),
            Key::Down => self.step(0, 1),
            Key::Home => self.cursor = (self.slots[0].row, self.slots[0].col),
            Key::End => {
                if let Some(i) = slot {
                    let s = self.slots[i];
                    let used = self.value(s.bind).trim_end().chars().count();
                    self.cursor.1 = s.col + used.min(s.len - 1);
                }
            }
            Key::EraseEof => self.edit(slot, |chars, off| chars.truncate(off)),
            Key::Delete => self.edit(slot, |chars, off| {
                if off < chars.len() {
                    chars.remove(off);
                }
            }),
            Key::Backspace => {
                if let Some(i) = slot {
                    if self.cursor.1 > self.slots[i].col {
                        self.cursor.1 -= 1;
                        self.edit(slot, |chars, off| {
                            if off < chars.len() {
                                chars.remove(off);
                            }
                        });
                    }
                }
            }
            Key::Enter => self.enter(slot, now),
            Key::Pf(n) if n == self.layout.keys.page_down => self.page(slot, true),
            Key::Pf(n) if n == self.layout.keys.page_up => self.page(slot, false),
            Key::Pf(n) => self.message = format!("PF{n} ist nicht belegt"),
        }
    }

    /// A click puts the cursor on the clicked cell, as terminal emulators do.
    /// Like a key, it is dropped while the host is busy.
    pub fn click(&mut self, row: usize, col: usize, now: Instant) {
        if !self.busy(now) && row < self.layout.rows && col < self.layout.cols {
            self.cursor = (row, col);
        }
    }

    fn slot_at(&self, (row, col): (usize, usize)) -> Option<usize> {
        self.slots
            .iter()
            .position(|s| s.row == row && col >= s.col && col < s.col + s.len)
    }

    fn next_start(&self) -> (usize, usize) {
        let s = self
            .slots
            .iter()
            .find(|s| (s.row, s.col) > self.cursor)
            .unwrap_or(&self.slots[0]);
        (s.row, s.col)
    }

    fn previous_start(&self, slot: Option<usize>) -> (usize, usize) {
        if let Some(i) = slot {
            let s = self.slots[i];
            if self.cursor.1 > s.col {
                return (s.row, s.col);
            }
        }
        let last = self.slots[self.slots.len() - 1];
        let s = self
            .slots
            .iter()
            .rev()
            .find(|s| (s.row, s.col) < self.cursor)
            .unwrap_or(&last);
        (s.row, s.col)
    }

    fn step(&mut self, dx: isize, dy: isize) {
        let (rows, cols) = (self.layout.rows as isize, self.layout.cols as isize);
        let at = self.cursor.0 as isize * cols + self.cursor.1 as isize + dx + dy * cols;
        let at = at.rem_euclid(rows * cols);
        self.cursor = ((at / cols) as usize, (at % cols) as usize);
    }

    fn overwrite(&mut self, i: usize, ch: char) {
        let s = self.slots[i];
        let off = self.cursor.1 - s.col;
        self.edit(Some(i), |chars, off| {
            chars.resize(chars.len().max(off + 1), ' ');
            chars[off] = ch;
        });
        // A full field skips to the next one, as a host's auto-skip does.
        if off + 1 == s.len {
            self.cursor = self.next_start();
        } else {
            self.cursor.1 += 1;
        }
    }

    fn edit(&mut self, slot: Option<usize>, change: impl FnOnce(&mut Vec<char>, usize)) {
        let Some(i) = slot else { return };
        let s = self.slots[i];
        let mut chars: Vec<char> = self.value(s.bind).chars().collect();
        change(&mut chars, self.cursor.1 - s.col);
        chars.truncate(s.len);
        let text: String = chars.into_iter().collect();
        self.set_value(s.bind, text.trim_end().to_string());
    }

    fn value(&self, bind: Bind) -> String {
        match bind {
            Bind::Field(i) => self.fields[i].clone(),
            Bind::Cell {
                table,
                line,
                column,
            } => {
                let t = &self.tables[table];
                t.rows
                    .get(t.offset + line)
                    .and_then(|r| r.get(column))
                    .cloned()
                    .unwrap_or_default()
            }
        }
    }

    fn set_value(&mut self, bind: Bind, text: String) {
        match bind {
            Bind::Field(i) => self.fields[i] = text,
            Bind::Cell {
                table,
                line,
                column,
            } => {
                let width = self.layout.tables[table].columns.len();
                let t = &mut self.tables[table];
                let index = t.offset + line;
                while t.rows.len() <= index {
                    t.rows.push(Vec::new());
                }
                let row = &mut t.rows[index];
                row.resize(row.len().max(width), String::new());
                row[column] = text;
            }
        }
    }

    fn table_of(&self, slot: Option<usize>) -> Option<usize> {
        match slot.map(|i| self.slots[i].bind) {
            Some(Bind::Cell { table, .. }) => Some(table),
            _ => None,
        }
    }

    fn enter(&mut self, slot: Option<usize>, now: Instant) {
        self.busy_until = Some(now + Duration::from_millis(self.layout.keys.enter_delay_ms));
        let table = self.table_of(slot);
        let (EnterMode::AddRow, Some(t)) = (self.layout.keys.enter, table) else {
            self.message = "Daten übernommen".into();
            return;
        };
        let rows = &mut self.tables[t].rows;
        if rows
            .last()
            .map_or(true, |r| r.iter().any(|v| !v.is_empty()))
        {
            rows.push(Vec::new());
        }
        let index = rows.len() - 1;
        let visible = self.layout.tables[t].visible_rows;
        let table = &mut self.tables[t];
        if index >= table.offset + visible {
            table.offset = index + 1 - visible;
        }
        let def = &self.layout.tables[t];
        self.cursor = (
            def.header_row + 1 + index - self.tables[t].offset,
            def.columns[0].col,
        );
        self.message = "Zeile angelegt".into();
    }

    fn page(&mut self, slot: Option<usize>, forward: bool) {
        let t = self.table_of(slot).unwrap_or(0);
        let Some(def) = self.layout.tables.get(t) else {
            return;
        };
        let visible = def.visible_rows;
        let table = &mut self.tables[t];
        if forward && table.offset + visible < table.rows.len() {
            table.offset += visible;
            self.message = String::new();
        } else if !forward && table.offset > 0 {
            table.offset = table.offset.saturating_sub(visible);
            self.message = String::new();
        } else {
            self.message = if forward {
                "Letzte Seite"
            } else {
                "Erste Seite"
            }
            .into();
        }
    }

    /// Every input field on screen as `(row, col, len)`.
    pub fn field_rects(&self) -> impl Iterator<Item = (usize, usize, usize)> + '_ {
        self.slots.iter().map(|s| (s.row, s.col, s.len))
    }

    /// The screen as text, one string per row, every row `cols` wide.
    pub fn grid(&self) -> Vec<String> {
        let mut grid = vec![vec![' '; self.layout.cols]; self.layout.rows];
        let mut put = |row: usize, col: usize, text: &str| {
            if let Some(line) = grid.get_mut(row) {
                for (i, ch) in text.chars().enumerate() {
                    if let Some(cell) = line.get_mut(col + i) {
                        *cell = ch;
                    }
                }
            }
        };
        for l in &self.layout.text {
            put(l.row, l.col, &l.text);
        }
        for t in &self.layout.tables {
            for c in &t.columns {
                put(t.header_row, c.col, &c.name);
            }
        }
        for s in &self.slots {
            put(s.row, s.col, &self.value(s.bind));
        }
        put(self.layout.message_row, 0, &self.message);
        grid.into_iter().map(|r| r.into_iter().collect()).collect()
    }

    pub fn status(&self, now: Instant) -> String {
        let dialog = if self.busy(now) {
            "Status wartet"
        } else {
            "Status bereit"
        };
        format!(
            "{dialog}      Ersetzen   {:02}, {:02}",
            self.cursor.0 + 1,
            self.cursor.1 + 1
        )
    }

    /// The ground truth an eval scores against.
    pub fn state(&self, now: Instant) -> Value {
        let mut fields = Map::new();
        for (f, v) in self.layout.fields.iter().zip(&self.fields) {
            fields.insert(f.name.clone(), json!(v));
        }
        let mut tables = Map::new();
        for (def, t) in self.layout.tables.iter().zip(&self.tables) {
            let rows: Vec<Map<String, Value>> = t
                .rows
                .iter()
                .map(|r| {
                    let cell = |i: usize| r.get(i).cloned().unwrap_or_default();
                    def.columns
                        .iter()
                        .enumerate()
                        .map(|(i, c)| (c.name.clone(), json!(cell(i))))
                        .collect()
                })
                .collect();
            tables.insert(
                def.name.clone(),
                json!({ "offset": t.offset, "rows": rows }),
            );
        }
        json!({
            "cursor": [self.cursor.0 + 1, self.cursor.1 + 1],
            "busy": self.busy(now),
            "message": self.message,
            "fields": fields,
            "tables": tables,
            "screen": self.grid(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAYOUT: &str = r#"
title: test
rows: 10
cols: 40
message_row: 9
fields:
  - { name: top, row: 0, col: 5, len: 4, value: AB }
tables:
  - name: grid
    header_row: 2
    visible_rows: 2
    columns:
      - { name: T, col: 1, len: 1 }
      - { name: Stufe, col: 4, len: 4 }
    rows:
      - ["1", XB30]
      - ["2", BX70]
      - ["3", XY10]
keys: { enter_delay_ms: 100 }
"#;

    fn screen() -> Screen {
        let layout: Layout = serde_yaml::from_str(LAYOUT).expect("layout parses");
        Screen::new(layout).expect("layout fits")
    }

    fn typed(s: &mut Screen, keys: &[Key]) {
        let now = Instant::now();
        for k in keys {
            s.press(*k, now);
        }
    }

    fn cell(s: &Screen, row: usize, column: &str) -> String {
        let state = s.state(Instant::now());
        state["tables"]["grid"]["rows"][row][column]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    #[test]
    fn tab_walks_fields_in_screen_order_and_wraps() {
        let mut s = screen();
        assert_eq!(s.cursor, (0, 5));
        typed(&mut s, &[Key::Tab]);
        assert_eq!(s.cursor, (3, 1));
        typed(&mut s, &[Key::Tab, Key::Tab]);
        assert_eq!(s.cursor, (4, 1));
        typed(&mut s, &[Key::Tab, Key::Tab]);
        assert_eq!(s.cursor, (0, 5));
        typed(&mut s, &[Key::BackTab]);
        assert_eq!(s.cursor, (4, 4));
    }

    #[test]
    fn typing_overwrites_and_a_full_field_skips_to_the_next() {
        let mut s = screen();
        typed(
            &mut s,
            &[Key::Tab, Key::Tab, Key::Char('B'), Key::Char('E')],
        );
        assert_eq!(cell(&s, 0, "Stufe"), "BE30", "overwrite keeps the old tail");
        typed(&mut s, &[Key::Char('9'), Key::Char('9')]);
        assert_eq!(cell(&s, 0, "Stufe"), "BE99");
        assert_eq!(
            s.cursor,
            (4, 1),
            "the full field skipped to the next line's K"
        );
    }

    #[test]
    fn erase_eof_clears_the_rest_of_the_field() {
        let mut s = screen();
        typed(&mut s, &[Key::Tab, Key::Tab, Key::Right, Key::EraseEof]);
        assert_eq!(cell(&s, 0, "Stufe"), "X");
    }

    #[test]
    fn enter_opens_a_line_scrolls_to_it_and_locks_the_keyboard() {
        let mut s = screen();
        let now = Instant::now();
        s.press(Key::Tab, now);
        s.press(Key::Enter, now);
        assert_eq!(s.state(now)["tables"]["grid"]["offset"], 2);
        assert_eq!(s.cursor, (4, 1), "the new line is the last visible one");
        s.press(Key::Char('7'), now);
        assert_eq!(
            cell(&s, 3, "T"),
            "",
            "a key during the round trip is dropped"
        );
        s.press(Key::Char('7'), now + Duration::from_millis(150));
        assert_eq!(cell(&s, 3, "T"), "7");
    }

    #[test]
    fn a_click_moves_the_cursor_and_typing_follows_it() {
        let mut s = screen();
        s.click(4, 5, Instant::now());
        typed(&mut s, &[Key::Char('Q')]);
        assert_eq!(cell(&s, 1, "Stufe"), "BQ70");
    }

    #[test]
    fn paging_scrolls_the_table_under_the_cursor() {
        let mut s = screen();
        typed(&mut s, &[Key::Tab, Key::Pf(8)]);
        assert!(s.grid()[3].contains("XY10"));
        typed(&mut s, &[Key::Pf(8)]);
        assert_eq!(s.message, "Letzte Seite");
        typed(&mut s, &[Key::Pf(7)]);
        assert!(s.grid()[3].contains("XB30"));
    }
}
