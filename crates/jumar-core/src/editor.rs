use std::collections::HashSet;

use crate::plan::{EntryId, Line};

/// Terminal-independent key, so the editor can be driven from tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Ctrl(char),
    Esc,
    Enter,
    Tab,
    Backspace,
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    /// Line-wise for both `v` and `V`: every line is a whole worktree.
    Visual,
    Insert,
    Command,
    Filter,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Write {
        quit: bool,
    },
    Quit,
    Open(EntryId),
    /// Hand these worktrees to an agent as context; `Tab`, on the cursor or selection.
    Context(Vec<EntryId>),
    /// The key pressed after the `<Space>` leader, on an existing worktree.
    Tool {
        key: char,
        id: EntryId,
    },
    Help,
    YankPath(EntryId),
    /// Rescan the worktrees, dropping any edits.
    Reload,
    /// An ex command the editor does not handle itself, such as `prune`.
    Command(String),
}

/// One worktree as the buffer first shows it. `keywords` is extra text the filter
/// matches, such as the repo and source.
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub keywords: String,
    /// Shown but not deletable or editable, like a repo's main checkout.
    pub fixed: bool,
}

const FIXED: &str = "the main checkout cannot be removed or renamed";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    None,
    Goto,
    Delete,
    Change,
    ChangeInner,
    Yank,
    Leader,
}

#[derive(Debug, Clone)]
struct Snapshot {
    lines: Vec<Line>,
    row: usize,
    col: usize,
}

#[derive(Debug)]
pub struct Editor {
    keywords: Vec<String>,
    fixed: Vec<bool>,
    original: Vec<Line>,
    lines: Vec<Line>,
    row: usize,
    col: usize,
    /// The line visual mode started on; the selection runs from here to `row`.
    anchor: usize,
    mode: Mode,
    pending: Pending,
    count: Option<usize>,
    prompt: String,
    filter: String,
    hidden: HashSet<EntryId>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    message: Option<String>,
}

impl Editor {
    #[must_use]
    pub fn new(entries: Vec<Entry>) -> Self {
        let mut keywords = Vec::with_capacity(entries.len());
        let mut lines = Vec::with_capacity(entries.len());
        let mut fixed = Vec::with_capacity(entries.len());
        for (index, entry) in entries.into_iter().enumerate() {
            keywords.push(entry.keywords.to_lowercase());
            fixed.push(entry.fixed);
            lines.push(Line::Existing {
                id: EntryId(index),
                name: entry.name,
            });
        }
        Self {
            keywords,
            fixed,
            original: lines.clone(),
            lines,
            row: 0,
            col: 0,
            anchor: 0,
            mode: Mode::Normal,
            pending: Pending::None,
            count: None,
            prompt: String::new(),
            filter: String::new(),
            hidden: HashSet::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            message: None,
        }
    }

    #[must_use]
    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    /// Lines the filter leaves visible, with their index into [`Editor::lines`].
    pub fn rows(&self) -> impl Iterator<Item = (usize, &Line)> {
        self.lines
            .iter()
            .enumerate()
            .filter(|(_, line)| self.is_visible(line))
    }

    /// `(line index, char column)`.
    #[must_use]
    pub fn cursor(&self) -> (usize, usize) {
        (self.row, self.col)
    }

    /// The selected range of line indices, inclusive, in visual mode. Lines the filter
    /// hides inside it are not part of the selection.
    #[must_use]
    pub fn selection(&self) -> Option<(usize, usize)> {
        (self.mode == Mode::Visual).then(|| (self.anchor.min(self.row), self.anchor.max(self.row)))
    }

    #[must_use]
    pub fn mode(&self) -> Mode {
        self.mode
    }

    #[must_use]
    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    #[must_use]
    pub fn filter(&self) -> &str {
        &self.filter
    }

    #[must_use]
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// True after `<Space>`, while the next key picks a tool.
    #[must_use]
    pub fn is_leader_pending(&self) -> bool {
        self.pending == Pending::Leader
    }

    /// The existing worktree under the cursor.
    #[must_use]
    pub fn cursor_entry(&self) -> Option<EntryId> {
        match self.current_row().map(|row| &self.lines[row]) {
            Some(Line::Existing { id, .. }) => Some(*id),
            _ => None,
        }
    }

    /// Takes over the filter of the editor this one replaces, with the cursor on `entry`.
    pub fn restore_view(&mut self, entry: Option<EntryId>, filter: &str) {
        if let Some(row) = entry.and_then(|entry| {
            self.lines
                .iter()
                .position(|line| matches!(line, Line::Existing { id, .. } if *id == entry))
        }) {
            self.row = row;
        }
        self.set_filter(filter.to_owned());
    }

    pub fn set_message(&mut self, message: impl Into<String>) {
        self.message = Some(message.into());
    }

    /// Deletes the visible lines of `ids` in one undoable step, skipping fixed ones.
    /// Returns how many were deleted.
    pub fn delete_entries(&mut self, ids: &HashSet<EntryId>) -> usize {
        let rows: Vec<usize> = self
            .rows()
            .filter(|(_, line)| matches!(line, Line::Existing { id, .. } if ids.contains(id)))
            .map(|(index, _)| index)
            .collect();
        let targets = self.deletable(&rows);
        if targets.is_empty() {
            return 0;
        }
        self.checkpoint();
        for &row in targets.iter().rev() {
            self.lines.remove(row);
        }
        let above = targets.iter().filter(|&&r| r < self.row).count();
        self.row = self.row.saturating_sub(above);
        self.col = 0;
        self.settle();
        targets.len()
    }

    /// Blank new lines are ignored, matching the planner.
    #[must_use]
    pub fn is_modified(&self) -> bool {
        let edited = self
            .lines
            .iter()
            .filter(|line| !matches!(line, Line::New { text, .. } if text.trim().is_empty()));
        !edited.eq(&self.original)
    }

    pub fn handle(&mut self, key: Key) -> Option<Action> {
        match self.mode {
            Mode::Normal | Mode::Visual => return self.normal(key),
            Mode::Insert => self.insert(key),
            Mode::Command => return self.command(key),
            Mode::Filter => self.filter_key(key),
        }
        None
    }

    fn normal(&mut self, key: Key) -> Option<Action> {
        self.message = None;
        let pending = std::mem::replace(&mut self.pending, Pending::None);
        if let Key::Char(c @ '0'..='9') = key
            && pending == Pending::None
            && (c != '0' || self.count.is_some())
        {
            let digit = c as usize - '0' as usize;
            let count = self.count.unwrap_or(0);
            self.count = Some(count.saturating_mul(10).saturating_add(digit));
            return None;
        }
        let count = self.count.take();
        let n = count.unwrap_or(1);
        match (pending, key) {
            (Pending::None, key) if self.mode == Mode::Visual => {
                return self.visual_key(key, count);
            }
            (Pending::None, key) => return self.normal_key(key, count),
            (Pending::Goto, Key::Char('g')) => self.goto(count.map_or(0, |c| c.saturating_sub(1))),
            (Pending::Delete, Key::Char('d')) => self.delete_lines(n),
            (Pending::Change, Key::Char('w')) => self.change_word(),
            (Pending::Change, Key::Char('i')) => self.pending = Pending::ChangeInner,
            (Pending::ChangeInner, Key::Char('w')) => self.change_inner_word(),
            (Pending::Yank, Key::Char('p')) => return self.cursor_entry().map(Action::YankPath),
            (Pending::Leader, Key::Char(key)) => {
                return self.cursor_entry().map(|id| Action::Tool { key, id });
            }
            _ => {}
        }
        None
    }

    fn normal_key(&mut self, key: Key, count: Option<usize>) -> Option<Action> {
        let n = count.unwrap_or(1);
        match key {
            Key::Char('j') | Key::Down => self.step(n, true),
            Key::Char('k') | Key::Up => self.step(n, false),
            Key::Char('h') | Key::Left => self.col = self.col.saturating_sub(n),
            Key::Char('l') | Key::Right => self.col = (self.col + n).min(self.max_col()),
            Key::Char('0') => self.col = 0,
            Key::Char('$') => self.col = self.max_col(),
            Key::Char('w') => {
                let chars = self.chars();
                for _ in 0..n {
                    self.col = next_word(&chars, self.col);
                }
            }
            Key::Char('b') => {
                let chars = self.chars();
                for _ in 0..n {
                    self.col = prev_word(&chars, self.col);
                }
            }
            Key::Char('G') => self.goto(count.map_or(usize::MAX, |c| c.saturating_sub(1))),
            Key::Char(c @ ('g' | 'd' | 'c')) => {
                self.pending = match c {
                    'g' => Pending::Goto,
                    'd' => Pending::Delete,
                    _ => Pending::Change,
                };
                self.count = count;
            }
            Key::Char(' ') => self.pending = Pending::Leader,
            Key::Char('y') => self.pending = Pending::Yank,
            Key::Char('R') => return self.reload(false),
            Key::Char('v' | 'V') if self.current_row().is_some() => {
                self.anchor = self.row;
                self.mode = Mode::Visual;
            }
            Key::Char('x') => self.delete_chars(n),
            Key::Char('o') => self.open_line(true),
            Key::Char('O') => self.open_line(false),
            Key::Char('i') => self.start_insert(self.col),
            Key::Char('a') => self.start_insert(self.col + 1),
            Key::Char('A') => self.start_insert(usize::MAX),
            Key::Char('I') => self.start_insert(0),
            Key::Char('u') => {
                for _ in 0..n {
                    self.undo();
                }
            }
            Key::Ctrl('r') => {
                for _ in 0..n {
                    self.redo();
                }
            }
            Key::Char('?') => return Some(Action::Help),
            Key::Char(':') => {
                self.prompt.clear();
                self.mode = Mode::Command;
            }
            Key::Char('/') => {
                self.set_filter(String::new());
                self.mode = Mode::Filter;
            }
            Key::Enter => {
                if let Some(id) = self.cursor_entry() {
                    return Some(Action::Open(id));
                }
            }
            Key::Tab => {
                if let Some(id) = self.cursor_entry() {
                    return Some(Action::Context(vec![id]));
                }
            }
            _ => {}
        }
        None
    }

    fn visual_key(&mut self, key: Key, count: Option<usize>) -> Option<Action> {
        match key {
            Key::Tab => return self.context_selection(),
            Key::Char('j' | 'k' | 'G' | 'g') | Key::Up | Key::Down => {
                self.normal_key(key, count);
            }
            Key::Char('o') => std::mem::swap(&mut self.anchor, &mut self.row),
            Key::Char('d' | 'x' | 'D') => self.delete_selection(),
            Key::Char('v' | 'V') | Key::Esc | Key::Ctrl('c') => self.mode = Mode::Normal,
            _ => {}
        }
        None
    }

    fn context_selection(&mut self) -> Option<Action> {
        let (_, rows) = self.take_selection()?;
        let ids: Vec<EntryId> = rows
            .into_iter()
            .filter_map(|row| match self.lines[row] {
                Line::Existing { id, .. } => Some(id),
                Line::New { .. } => None,
            })
            .collect();
        (!ids.is_empty()).then_some(Action::Context(ids))
    }

    /// Ends visual mode, returning where the selection starts and the visible lines in it.
    fn take_selection(&mut self) -> Option<(usize, Vec<usize>)> {
        let (first, last) = self.selection()?;
        self.mode = Mode::Normal;
        let rows = self
            .visible()
            .into_iter()
            .filter(|row| (first..=last).contains(row))
            .collect();
        Some((first, rows))
    }

    fn insert(&mut self, key: Key) {
        match key {
            Key::Char(c) => {
                let col = self.col;
                self.edit_current(|chars| chars.insert(col, c));
                self.col += 1;
            }
            Key::Backspace if self.col > 0 => {
                let col = self.col - 1;
                self.edit_current(|chars| {
                    chars.remove(col);
                });
                self.col = col;
            }
            Key::Left => self.col = self.col.saturating_sub(1),
            Key::Right => self.col = (self.col + 1).min(self.len()),
            Key::Esc | Key::Enter | Key::Ctrl('c') => self.finish_insert(),
            _ => {}
        }
    }

    fn reload(&mut self, discard: bool) -> Option<Action> {
        if self.is_modified() && !discard {
            self.set_message("unsaved changes (:e! to discard)");
            return None;
        }
        Some(Action::Reload)
    }

    fn command(&mut self, key: Key) -> Option<Action> {
        match key {
            Key::Char(c) => self.prompt.push(c),
            Key::Backspace => {
                if self.prompt.pop().is_none() {
                    self.mode = Mode::Normal;
                }
            }
            Key::Esc | Key::Ctrl('c') => self.mode = Mode::Normal,
            Key::Enter => {
                self.mode = Mode::Normal;
                let command = std::mem::take(&mut self.prompt);
                return match command.trim() {
                    "" => None,
                    "w" => Some(Action::Write { quit: false }),
                    "wq" | "x" => Some(Action::Write { quit: true }),
                    "q" if self.is_modified() => {
                        self.set_message("unsaved changes (:q! to discard)");
                        None
                    }
                    "q" | "q!" => Some(Action::Quit),
                    "h" | "help" => Some(Action::Help),
                    "e" => self.reload(false),
                    "e!" => self.reload(true),
                    other => Some(Action::Command(other.to_owned())),
                };
            }
            _ => {}
        }
        None
    }

    fn filter_key(&mut self, key: Key) {
        match key {
            Key::Char(c) => {
                let mut query = std::mem::take(&mut self.filter);
                query.push(c);
                self.set_filter(query);
            }
            Key::Backspace => {
                let mut query = std::mem::take(&mut self.filter);
                if query.pop().is_none() {
                    self.mode = Mode::Normal;
                }
                self.set_filter(query);
            }
            Key::Enter => self.mode = Mode::Normal,
            Key::Esc | Key::Ctrl('c') => {
                self.set_filter(String::new());
                self.mode = Mode::Normal;
            }
            _ => {}
        }
    }

    // Visibility is decided when the filter changes, so a line being renamed never
    // vanishes from under the cursor.
    fn set_filter(&mut self, query: String) {
        let needle = query.to_lowercase();
        self.hidden = self
            .lines
            .iter()
            .filter_map(|line| match line {
                Line::Existing { id, name }
                    if !name.to_lowercase().contains(&needle)
                        && !self.keywords[id.0].contains(&needle) =>
                {
                    Some(*id)
                }
                _ => None,
            })
            .collect();
        self.filter = query;
        self.settle();
    }

    fn is_visible(&self, line: &Line) -> bool {
        match line {
            Line::Existing { id, .. } => !self.hidden.contains(id),
            Line::New { .. } => true,
        }
    }

    fn visible(&self) -> Vec<usize> {
        self.rows().map(|(index, _)| index).collect()
    }

    fn is_fixed(&self, row: usize) -> bool {
        matches!(&self.lines[row], Line::Existing { id, .. } if self.fixed[id.0])
    }

    /// The current row when it may be edited; a fixed one says so instead.
    fn editable_row(&mut self) -> Option<usize> {
        let row = self.current_row()?;
        if self.is_fixed(row) {
            self.set_message(FIXED);
            return None;
        }
        Some(row)
    }

    /// Drops fixed rows from `targets`, saying so when that leaves nothing.
    fn deletable(&mut self, targets: &[usize]) -> Vec<usize> {
        let kept: Vec<usize> = targets
            .iter()
            .copied()
            .filter(|&r| !self.is_fixed(r))
            .collect();
        if kept.is_empty() && !targets.is_empty() {
            self.set_message(FIXED);
        }
        kept
    }

    fn current_row(&self) -> Option<usize> {
        self.lines
            .get(self.row)
            .filter(|line| self.is_visible(line))
            .map(|_| self.row)
    }

    fn chars(&self) -> Vec<char> {
        self.current_row()
            .map(|row| text(&self.lines[row]).chars().collect())
            .unwrap_or_default()
    }

    fn len(&self) -> usize {
        self.current_row()
            .map_or(0, |row| text(&self.lines[row]).chars().count())
    }

    fn max_col(&self) -> usize {
        self.len().saturating_sub(1)
    }

    fn edit_current(&mut self, edit: impl FnOnce(&mut Vec<char>)) {
        if let Some(row) = self.current_row() {
            let target = text_mut(&mut self.lines[row]);
            let mut chars: Vec<char> = target.chars().collect();
            edit(&mut chars);
            *target = chars.into_iter().collect();
        }
    }

    /// Moves the cursor onto a visible line and clamps the column for the mode.
    fn settle(&mut self) {
        if self.row >= self.lines.len() {
            self.row = self.lines.len().saturating_sub(1);
        }
        let visible = self.visible();
        if let Some(&row) = visible
            .iter()
            .find(|&&r| r >= self.row)
            .or_else(|| visible.last())
        {
            self.row = row;
        }
        let max = if self.mode == Mode::Insert {
            self.len()
        } else {
            self.max_col()
        };
        self.col = self.col.min(max);
    }

    fn step(&mut self, n: usize, down: bool) {
        let visible = self.visible();
        if let Some(pos) = visible.iter().position(|&r| r == self.row) {
            let pos = if down {
                pos.saturating_add(n)
            } else {
                pos.saturating_sub(n)
            };
            self.goto(pos);
        }
    }

    fn goto(&mut self, pos: usize) {
        let visible = self.visible();
        if let Some(&row) = visible.get(pos).or_else(|| visible.last()) {
            self.row = row;
            self.settle();
        }
    }

    fn checkpoint(&mut self) {
        self.undo.push(Snapshot {
            lines: self.lines.clone(),
            row: self.row,
            col: self.col,
        });
        self.redo.clear();
    }

    fn restore(&mut self, snapshot: Snapshot) -> Snapshot {
        let current = Snapshot {
            lines: std::mem::replace(&mut self.lines, snapshot.lines),
            row: self.row,
            col: self.col,
        };
        self.row = snapshot.row;
        self.col = snapshot.col;
        self.settle();
        current
    }

    fn undo(&mut self) {
        match self.undo.pop() {
            Some(snapshot) => {
                let current = self.restore(snapshot);
                self.redo.push(current);
            }
            None => self.set_message("already at oldest change"),
        }
    }

    fn redo(&mut self) {
        match self.redo.pop() {
            Some(snapshot) => {
                let current = self.restore(snapshot);
                self.undo.push(current);
            }
            None => self.set_message("already at newest change"),
        }
    }

    fn delete_lines(&mut self, n: usize) {
        let visible = self.visible();
        let Some(pos) = visible.iter().position(|&r| r == self.row) else {
            return;
        };
        let span = &visible[pos..visible.len().min(pos.saturating_add(n))];
        let targets = self.deletable(span);
        let Some(&first) = targets.first() else {
            return;
        };
        self.checkpoint();
        for &row in targets.iter().rev() {
            self.lines.remove(row);
        }
        self.row = first;
        self.col = 0;
        self.settle();
    }

    fn delete_selection(&mut self) {
        let Some((first, selected)) = self.take_selection() else {
            return;
        };
        let targets = self.deletable(&selected);
        if targets.is_empty() {
            return;
        }
        // Undo returns to where the selection started, as in vim.
        self.row = first;
        self.checkpoint();
        for &row in targets.iter().rev() {
            self.lines.remove(row);
        }
        self.col = 0;
        self.settle();
    }

    fn delete_chars(&mut self, n: usize) {
        if self.editable_row().is_none() {
            return;
        }
        let len = self.len();
        if self.col >= len {
            return;
        }
        self.checkpoint();
        let range = self.col..len.min(self.col.saturating_add(n));
        self.edit_current(|chars| {
            chars.drain(range);
        });
        self.settle();
    }

    fn change_word(&mut self) {
        let chars = self.chars();
        let end = if chars.is_empty() {
            0
        } else {
            run_end(&chars, self.col)
        };
        self.change(self.col, end);
    }

    fn change_inner_word(&mut self) {
        let chars = self.chars();
        if chars.is_empty() {
            self.change(0, 0);
        } else {
            self.change(run_start(&chars, self.col), run_end(&chars, self.col));
        }
    }

    fn change(&mut self, start: usize, end: usize) {
        if self.editable_row().is_none() {
            return;
        }
        self.checkpoint();
        self.edit_current(|chars| {
            chars.drain(start..end);
        });
        self.col = start;
        self.mode = Mode::Insert;
    }

    fn open_line(&mut self, below: bool) {
        let near = self.current_row().and_then(|row| match &self.lines[row] {
            Line::Existing { id, .. } => Some(*id),
            Line::New { near, .. } => *near,
        });
        let index = match self.current_row() {
            Some(row) if below => row + 1,
            Some(row) => row,
            None => self.lines.len(),
        };
        self.checkpoint();
        self.lines.insert(
            index,
            Line::New {
                text: String::new(),
                near,
            },
        );
        self.row = index;
        self.col = 0;
        self.mode = Mode::Insert;
    }

    fn start_insert(&mut self, col: usize) {
        if self.editable_row().is_none() {
            return;
        }
        self.checkpoint();
        self.mode = Mode::Insert;
        self.col = col.min(self.len());
    }

    fn finish_insert(&mut self) {
        self.mode = Mode::Normal;
        self.col = self.col.saturating_sub(1);
        self.settle();
        if self
            .undo
            .last()
            .is_some_and(|snapshot| snapshot.lines == self.lines)
        {
            self.undo.pop();
        }
    }
}

/// The editable text of a line: the directory name, or the raw new-line text.
#[must_use]
pub fn text(line: &Line) -> &str {
    match line {
        Line::Existing { name, .. } => name,
        Line::New { text, .. } => text,
    }
}

fn text_mut(line: &mut Line) -> &mut String {
    match line {
        Line::Existing { name, .. } => name,
        Line::New { text, .. } => text,
    }
}

#[derive(PartialEq, Eq)]
enum Class {
    Blank,
    Word,
    Punct,
}

fn class(c: char) -> Class {
    if c.is_whitespace() {
        Class::Blank
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

fn run_start(chars: &[char], col: usize) -> usize {
    let col = col.min(chars.len() - 1);
    let class = class(chars[col]);
    chars[..col]
        .iter()
        .rposition(|&c| self::class(c) != class)
        .map_or(0, |i| i + 1)
}

fn run_end(chars: &[char], col: usize) -> usize {
    let col = col.min(chars.len() - 1);
    let class = class(chars[col]);
    chars[col..]
        .iter()
        .position(|&c| self::class(c) != class)
        .map_or(chars.len(), |i| col + i)
}

fn next_word(chars: &[char], col: usize) -> usize {
    let mut i = col;
    if let Some(&c) = chars.get(i)
        && class(c) != Class::Blank
    {
        i = run_end(chars, i);
    }
    while chars.get(i).is_some_and(|&c| class(c) == Class::Blank) {
        i += 1;
    }
    if i >= chars.len() {
        chars.len().saturating_sub(1)
    } else {
        i
    }
}

fn prev_word(chars: &[char], col: usize) -> usize {
    let mut i = col.min(chars.len());
    while i > 0 && class(chars[i - 1]) == Class::Blank {
        i -= 1;
    }
    if i > 0 {
        i = run_start(chars, i - 1);
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor(names: &[&str]) -> Editor {
        Editor::new(
            names
                .iter()
                .map(|name| Entry {
                    name: (*name).into(),
                    keywords: format!("#{name}#"),
                    fixed: name.starts_with('('),
                })
                .collect(),
        )
    }

    /// Feeds vim-notation keys (`<Esc>`, `<CR>`, `<BS>`, `<C-r>`) and returns the last
    /// action produced.
    fn feed(editor: &mut Editor, keys: &str) -> Option<Action> {
        let mut action = None;
        let mut rest = keys;
        while let Some(c) = rest.chars().next() {
            let special = rest
                .strip_prefix('<')
                .and_then(|r| r.split_once('>'))
                .and_then(|(name, after)| {
                    let key = match name {
                        "Esc" => Key::Esc,
                        "CR" => Key::Enter,
                        "BS" => Key::Backspace,
                        "Tab" => Key::Tab,
                        "C-r" => Key::Ctrl('r'),
                        _ => return None,
                    };
                    Some((key, after))
                });
            let key = if let Some((key, after)) = special {
                rest = after;
                key
            } else {
                rest = &rest[c.len_utf8()..];
                Key::Char(c)
            };
            if let Some(produced) = editor.handle(key) {
                action = Some(produced);
            }
        }
        action
    }

    #[test]
    fn delete_entries_should_skip_lines_the_filter_hides() {
        let mut ed = editor(&["a", "b", "c"]);
        let ids = [0, 1, 2].map(EntryId).into_iter().collect();
        feed(&mut ed, "/c<CR>");

        let deleted = ed.delete_entries(&ids);

        assert_eq!((deleted, texts(&ed)), (1, vec!["a", "b"]));
    }

    #[test]
    fn delete_entries_should_skip_fixed_lines() {
        let mut ed = editor(&["(main)", "a", "b"]);
        let ids = [0, 1, 2].map(EntryId).into_iter().collect();

        let deleted = ed.delete_entries(&ids);

        assert_eq!((deleted, texts(&ed)), (2, vec!["(main)"]));
    }

    #[test]
    fn delete_entries_should_undo_in_one_step() {
        let mut ed = editor(&["a", "b", "c"]);
        let ids = [EntryId(0), EntryId(2)].into_iter().collect();
        ed.delete_entries(&ids);

        feed(&mut ed, "u");

        assert_eq!(texts(&ed), ["a", "b", "c"]);
    }

    fn texts(editor: &Editor) -> Vec<&str> {
        editor.lines().iter().map(text).collect()
    }

    fn visible(editor: &Editor) -> Vec<&str> {
        editor.rows().map(|(_, line)| text(line)).collect()
    }

    fn row_after(names: &[&str], keys: &str) -> usize {
        let mut ed = editor(names);
        feed(&mut ed, keys);
        ed.cursor().0
    }

    fn col_after(name: &str, keys: &str) -> usize {
        let mut ed = editor(&[name]);
        feed(&mut ed, keys);
        ed.cursor().1
    }

    fn texts_after(names: &[&str], keys: &str) -> Vec<String> {
        let mut ed = editor(names);
        feed(&mut ed, keys);
        texts(&ed).into_iter().map(str::to_owned).collect()
    }

    const ABC: [&str; 3] = ["a", "b", "c"];

    #[test]
    fn j_should_move_down() {
        assert_eq!(row_after(&ABC, "j"), 1);
    }

    #[test]
    fn j_should_take_a_count() {
        assert_eq!(row_after(&ABC, "2j"), 2);
    }

    #[test]
    fn j_should_stop_at_last_line() {
        assert_eq!(row_after(&ABC, "9j"), 2);
    }

    #[test]
    fn k_should_move_up() {
        assert_eq!(row_after(&ABC, "jjk"), 1);
    }

    #[test]
    fn capital_g_should_go_to_last_line() {
        assert_eq!(row_after(&ABC, "G"), 2);
    }

    #[test]
    fn capital_g_should_go_to_counted_line() {
        assert_eq!(row_after(&ABC, "2G"), 1);
    }

    #[test]
    fn gg_should_go_to_first_line() {
        assert_eq!(row_after(&ABC, "Ggg"), 0);
    }

    #[test]
    fn dd_should_delete_line() {
        assert_eq!(texts_after(&ABC, "jdd"), ["a", "c"]);
    }

    #[test]
    fn dd_should_take_a_count() {
        assert_eq!(texts_after(&ABC, "2dd"), ["c"]);
    }

    #[test]
    fn dd_should_move_up_when_last_line_deleted() {
        assert_eq!(row_after(&ABC, "Gdd"), 1);
    }

    #[test]
    fn dd_should_keep_entry_ids_of_remaining_lines() {
        let mut ed = editor(&ABC);

        feed(&mut ed, "dd");

        assert!(matches!(
            ed.lines()[0],
            Line::Existing { id: EntryId(1), .. }
        ));
    }

    #[test]
    fn dd_should_keep_fixed_line() {
        assert_eq!(texts_after(&["(main)", "a"], "dd"), ["(main)", "a"]);
    }

    #[test]
    fn visual_d_should_skip_fixed_line() {
        assert_eq!(texts_after(&["(main)", "a", "b"], "Vjd"), ["(main)", "b"]);
    }

    #[test]
    fn insert_should_refuse_fixed_line() {
        assert_eq!(texts_after(&["(main)"], "Ax<Esc>"), ["(main)"]);
    }

    #[test]
    fn yp_should_yank_current_worktree_path() {
        assert_eq!(
            feed(&mut editor(&ABC), "jyp"),
            Some(Action::YankPath(EntryId(1)))
        );
    }

    #[test]
    fn reload_should_refuse_with_unsaved_changes() {
        assert_eq!(feed(&mut editor(&ABC), "ddR"), None);
    }

    #[test]
    fn e_bang_should_reload_despite_unsaved_changes() {
        assert_eq!(feed(&mut editor(&ABC), "dd:e!<CR>"), Some(Action::Reload));
    }

    #[test]
    fn question_mark_should_ask_for_help() {
        assert_eq!(feed(&mut editor(&ABC), "?"), Some(Action::Help));
    }

    #[test]
    fn visual_d_should_delete_selected_lines() {
        assert_eq!(texts_after(&["a", "b", "c", "d"], "jVjd"), ["a", "d"]);
    }

    #[test]
    fn visual_should_select_upwards() {
        assert_eq!(texts_after(&["a", "b", "c", "d"], "Gvkkd"), ["a"]);
    }

    #[test]
    fn visual_o_should_swap_ends() {
        let mut ed = editor(&["a", "b", "c", "d"]);
        feed(&mut ed, "jVjok");

        assert_eq!(ed.selection(), Some((0, 2)));
    }

    #[test]
    fn visual_should_take_counts() {
        assert_eq!(texts_after(&["a", "b", "c", "d"], "V2jx"), ["d"]);
    }

    #[test]
    fn visual_delete_should_skip_lines_hidden_by_the_filter() {
        let mut ed = editor(&["ab", "c", "ad"]);
        feed(&mut ed, "/a<CR>Vjd/<Esc>");

        assert_eq!(texts(&ed), ["c"]);
    }

    #[test]
    fn visual_esc_should_leave_lines_alone() {
        assert_eq!(texts_after(&ABC, "Vj<Esc>d"), ABC);
    }

    #[test]
    fn visual_delete_should_undo_in_one_step() {
        assert_eq!(
            texts_after(&["a", "b", "c", "d"], "jVjdu"),
            ["a", "b", "c", "d"]
        );
    }

    #[test]
    fn x_should_delete_char_under_cursor() {
        assert_eq!(texts_after(&["abc"], "lx"), ["ac"]);
    }

    #[test]
    fn x_should_take_a_count() {
        assert_eq!(texts_after(&["abcd"], "2x"), ["cd"]);
    }

    #[test]
    fn x_should_keep_cursor_on_line_when_deleting_last_char() {
        assert_eq!(col_after("abc", "$x"), 1);
    }

    #[test]
    fn cw_should_change_to_end_of_word() {
        assert_eq!(texts_after(&["fix-auth"], "cwbug<Esc>"), ["bug-auth"]);
    }

    #[test]
    fn ciw_should_change_whole_word_under_cursor() {
        assert_eq!(texts_after(&["fix-auth"], "6lciwlogin<Esc>"), ["fix-login"]);
    }

    #[test]
    fn rename_should_keep_entry_id() {
        let mut ed = editor(&["a"]);

        feed(&mut ed, "A2<Esc>");

        let expected = Line::Existing {
            id: EntryId(0),
            name: "a2".into(),
        };
        assert_eq!(ed.lines(), [expected]);
    }

    #[test]
    fn i_should_insert_before_cursor() {
        assert_eq!(texts_after(&["ac"], "lib<Esc>"), ["abc"]);
    }

    #[test]
    fn a_should_insert_after_cursor() {
        assert_eq!(texts_after(&["ac"], "ab<Esc>"), ["abc"]);
    }

    #[test]
    fn capital_i_should_insert_at_line_start() {
        assert_eq!(texts_after(&["bc"], "$Ia<Esc>"), ["abc"]);
    }

    #[test]
    fn backspace_should_delete_before_cursor_in_insert_mode() {
        assert_eq!(texts_after(&["abc"], "A<BS><Esc>"), ["ab"]);
    }

    #[test]
    fn esc_should_move_cursor_left() {
        assert_eq!(col_after("abc", "A<Esc>"), 2);
    }

    #[test]
    fn o_should_open_new_line_below() {
        let mut ed = editor(&ABC);

        feed(&mut ed, "ofix<Esc>");

        let expected = Line::New {
            text: "fix".into(),
            near: Some(EntryId(0)),
        };
        assert_eq!(ed.lines()[1], expected);
    }

    #[test]
    fn capital_o_should_take_the_cursor_line_as_near() {
        let mut ed = editor(&ABC);

        feed(&mut ed, "jOz<Esc>");

        assert!(matches!(
            ed.lines()[1],
            Line::New {
                near: Some(EntryId(1)),
                ..
            }
        ));
    }

    #[test]
    fn o_on_a_new_line_should_keep_its_near() {
        let mut ed = editor(&ABC);

        feed(&mut ed, "jOz<Esc>oy<Esc>");

        assert!(matches!(
            ed.lines()[2],
            Line::New {
                near: Some(EntryId(1)),
                ..
            }
        ));
    }

    #[test]
    fn capital_o_should_open_new_line_above() {
        assert_eq!(texts_after(&ABC, "jOz<Esc>"), ["a", "z", "b", "c"]);
    }

    #[test]
    fn o_should_work_in_empty_buffer() {
        assert_eq!(texts_after(&[], "oz<Esc>"), ["z"]);
    }

    #[test]
    fn w_should_move_to_next_word() {
        assert_eq!(col_after("fix-auth", "w"), 3);
    }

    #[test]
    fn w_should_take_a_count() {
        assert_eq!(col_after("fix-auth", "2w"), 4);
    }

    #[test]
    fn b_should_move_to_previous_word() {
        assert_eq!(col_after("fix-auth", "$b"), 4);
    }

    #[test]
    fn zero_should_move_to_line_start() {
        assert_eq!(col_after("abc", "$0"), 0);
    }

    #[test]
    fn zero_should_extend_a_count() {
        let names: Vec<String> = (0..12).map(|i| i.to_string()).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();

        assert_eq!(row_after(&names, "10j"), 10);
    }

    #[test]
    fn u_should_undo_whole_insert_session() {
        assert_eq!(texts_after(&["a"], "Abcd<Esc>u"), ["a"]);
    }

    #[test]
    fn u_should_restore_deleted_lines() {
        assert_eq!(texts_after(&ABC, "2ddu"), ABC);
    }

    #[test]
    fn u_should_skip_insert_sessions_without_changes() {
        assert_eq!(texts_after(&["ab"], "xi<Esc>u"), ["ab"]);
    }

    #[test]
    fn ctrl_r_should_redo() {
        assert_eq!(texts_after(&ABC, "ddu<C-r>"), ["b", "c"]);
    }

    #[test]
    fn new_change_should_clear_redo() {
        assert_eq!(texts_after(&ABC, "ddux<C-r>"), ["", "b", "c"]);
    }

    #[test]
    fn u_should_report_when_nothing_to_undo() {
        let mut ed = editor(&ABC);

        feed(&mut ed, "u");

        assert_eq!(ed.message(), Some("already at oldest change"));
    }

    #[test]
    fn colon_w_should_write() {
        assert_eq!(
            feed(&mut editor(&ABC), ":w<CR>"),
            Some(Action::Write { quit: false })
        );
    }

    #[test]
    fn colon_wq_should_write_and_quit() {
        assert_eq!(
            feed(&mut editor(&ABC), ":wq<CR>"),
            Some(Action::Write { quit: true })
        );
    }

    #[test]
    fn colon_q_should_quit_when_unmodified() {
        assert_eq!(feed(&mut editor(&ABC), ":q<CR>"), Some(Action::Quit));
    }

    #[test]
    fn colon_q_should_refuse_when_modified() {
        assert_eq!(feed(&mut editor(&ABC), "dd:q<CR>"), None);
    }

    #[test]
    fn colon_q_bang_should_quit_when_modified() {
        assert_eq!(feed(&mut editor(&ABC), "dd:q!<CR>"), Some(Action::Quit));
    }

    #[test]
    fn unknown_command_should_be_passed_on() {
        assert_eq!(
            feed(&mut editor(&ABC), ":prune<CR>"),
            Some(Action::Command("prune".into()))
        );
    }

    #[test]
    fn esc_should_cancel_command() {
        assert_eq!(feed(&mut editor(&ABC), ":w<Esc>"), None);
    }

    #[test]
    fn enter_should_open_existing_worktree() {
        assert_eq!(
            feed(&mut editor(&ABC), "j<CR>"),
            Some(Action::Open(EntryId(1)))
        );
    }

    #[test]
    fn leader_should_pick_tool_for_current_worktree() {
        assert_eq!(
            feed(&mut editor(&ABC), "j d"),
            Some(Action::Tool {
                key: 'd',
                id: EntryId(1)
            })
        );
    }

    #[test]
    fn leader_should_ignore_new_line() {
        assert_eq!(feed(&mut editor(&ABC), "oz<Esc> d"), None);
    }

    #[test]
    fn leader_should_consume_the_key_after_it() {
        assert_eq!(texts_after(&ABC, " dd"), ABC);
    }

    #[test]
    fn enter_should_ignore_new_line() {
        assert_eq!(feed(&mut editor(&ABC), "oz<Esc><CR>"), None);
    }

    #[test]
    fn is_modified_should_be_false_initially() {
        assert!(!editor(&ABC).is_modified());
    }

    #[test]
    fn is_modified_should_be_true_after_rename() {
        let mut ed = editor(&ABC);

        feed(&mut ed, "Ax<Esc>");

        assert!(ed.is_modified());
    }

    #[test]
    fn is_modified_should_ignore_blank_new_lines() {
        let mut ed = editor(&ABC);

        feed(&mut ed, "o<Esc>");

        assert!(!ed.is_modified());
    }

    #[test]
    fn filter_should_hide_non_matching_lines() {
        let mut ed = editor(&["fix", "feat", "fox"]);

        feed(&mut ed, "/f<BS>o<CR>");

        assert_eq!(visible(&ed), ["fox"]);
    }

    #[test]
    fn filter_should_match_keywords() {
        let mut ed = editor(&ABC);

        feed(&mut ed, "/#b#<CR>");

        assert_eq!(visible(&ed), ["b"]);
    }

    #[test]
    fn filter_should_ignore_case() {
        let mut ed = editor(&["Fix", "feat"]);

        feed(&mut ed, "/FI<CR>");

        assert_eq!(visible(&ed), ["Fix"]);
    }

    #[test]
    fn filter_should_move_cursor_onto_visible_line() {
        assert_eq!(row_after(&ABC, "/c<CR>"), 2);
    }

    #[test]
    fn esc_should_clear_filter() {
        let mut ed = editor(&ABC);

        feed(&mut ed, "/c<Esc>");

        assert_eq!(visible(&ed), ABC);
    }

    #[test]
    fn j_should_skip_hidden_lines() {
        assert_eq!(row_after(&["fa", "b", "fc"], "/f<CR>j"), 2);
    }

    #[test]
    fn dd_should_only_delete_visible_lines() {
        assert_eq!(texts_after(&["fa", "b", "fc"], "/f<CR>2dd"), ["b"]);
    }

    #[test]
    fn new_lines_should_stay_visible_under_filter() {
        let mut ed = editor(&["fa", "b"]);

        feed(&mut ed, "/f<CR>oz<Esc>");

        assert_eq!(visible(&ed), ["fa", "z"]);
    }

    #[test]
    fn renamed_line_should_stay_visible_under_filter() {
        let mut ed = editor(&["fa", "b"]);

        feed(&mut ed, "/f<CR>ciwz<Esc>");

        assert_eq!(visible(&ed), ["z"]);
    }

    #[test]
    fn dd_should_do_nothing_when_everything_is_filtered_out() {
        assert_eq!(texts_after(&ABC, "/zzz<CR>dd"), ABC);
    }

    #[test]
    fn restore_view_should_put_cursor_on_entry() {
        let mut editor = editor(&["a", "b", "c"]);

        editor.restore_view(Some(EntryId(2)), "");

        assert_eq!(editor.cursor_entry(), Some(EntryId(2)));
    }

    #[test]
    fn restore_view_should_reapply_filter() {
        let mut editor = editor(&["a", "b", "c"]);

        editor.restore_view(None, "b");

        assert_eq!(editor.rows().count(), 1);
    }

    #[test]
    fn tab_should_give_cursor_entry_as_context() {
        let mut editor = editor(&["a", "b"]);

        let action = feed(&mut editor, "j<Tab>");

        assert_eq!(action, Some(Action::Context(vec![EntryId(1)])));
    }

    #[test]
    fn visual_tab_should_give_selected_entries_as_context() {
        let mut editor = editor(&["a", "b", "c"]);

        let action = feed(&mut editor, "Vj<Tab>");

        assert_eq!(action, Some(Action::Context(vec![EntryId(0), EntryId(1)])));
    }
}
