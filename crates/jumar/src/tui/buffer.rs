use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use jumar_core::config::Tool;
use jumar_core::editor::{Editor, Entry, Mode};
use jumar_core::herdr::{self, AgentStatus, Herdr};
use jumar_core::inspect::inspect_all;
use jumar_core::model::{Head, Health, Worktree};
use jumar_core::plan::{self, EntryId, Line as BufferLine};
use jumar_core::safety::Tier;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::{column_width, hints, mode_tag, pad, theme, to_u16, truncate, width, window};
use crate::discover::{self, Scan};
use crate::format::{self, InspectResult};

/// The editable worktree list, with inspection results filling in as they arrive.
pub struct Buffer {
    pub worktrees: Arc<[Worktree]>,
    pub editor: Editor,
    results: Vec<Option<InspectResult>>,
    /// Worktrees other than main checkouts, for the title.
    linked: usize,
    loading: usize,
    inspections: Receiver<(usize, InspectResult)>,
    scroll: usize,
    tool_hint: Vec<(String, String)>,
    herdr: Option<herdr::State>,
    herdr_updates: Receiver<herdr::State>,
    home: PathBuf,
}

/// Display text for one cell of each column, computed once per frame.
struct Row<'a> {
    /// `None` for a new line, which shows under whichever group it was typed in.
    repo: Option<&'a Path>,
    mark: Span<'static>,
    /// The line's whole text, which the cursor moves through.
    text: &'a str,
    name: &'a str,
    /// The typed branch within `text`; `None` when the line has no branch to edit.
    branch_text: Option<&'a str>,
    branch: String,
    branch_style: Style,
    /// What a new line will create, or why it can't, shown dim beneath it.
    hint: Option<Span<'static>>,
    state: String,
    age: Span<'static>,
    source: &'a str,
    herdr: Span<'static>,
    style: Style,
    state_style: Style,
}

/// One line of the scrolling list.
enum Display<'a> {
    Gap,
    Group(&'a Path),
    Row(usize),
    /// One line of a new line's hint, wrapped to fit; the first carries the arrow.
    Hint {
        text: String,
        style: Style,
        first: bool,
    },
}

const HEADERS: [&str; 6] = ["NAME", "BRANCH", "STATUS", "AGE", "SOURCE", "HERDR"];
/// Worktrees whose last commit is older than this show their age in yellow.
const STALE_SECONDS: i64 = 30 * 24 * 60 * 60;
const GAP: &str = "   ";
/// Rows sit under their group's title; the mark column comes first.
const INDENT: &str = "  ";
const MARK_WIDTH: usize = 2;
const ARROW: &str = "↳ ";
/// Columns cut to fit the frame keep at least this many columns.
const MIN_CUT: usize = 8;

impl Buffer {
    pub fn new(scan: Scan, tools: &[Tool]) -> Self {
        let worktrees: Arc<[Worktree]> = scan.worktrees.into();
        let entries = worktrees
            .iter()
            .map(|w| Entry {
                text: entry_text(w),
                fixed: w.main,
                keywords: format!(
                    "{} {} {}",
                    format::file_name(&w.repo),
                    format::source(w),
                    format::branch(w.head.as_ref()),
                ),
            })
            .collect();
        let mut editor = Editor::new(entries);
        if let Some(warning) = scan.warnings.first() {
            editor.set_message(warning.clone());
        }
        Self {
            results: std::iter::repeat_with(|| None)
                .take(worktrees.len())
                .collect(),
            loading: worktrees.len(),
            linked: worktrees.iter().filter(|w| !w.main).count(),
            inspections: inspect_all(&worktrees, discover::WORKERS),
            editor,
            worktrees,
            scroll: 0,
            tool_hint: tools
                .iter()
                .map(|t| (t.key.to_string(), t.name.clone()))
                .collect(),
            herdr: None,
            herdr_updates: read_herdr(),
            home: std::env::home_dir().unwrap_or_default(),
        }
    }

    /// A buffer for `scan` that keeps this one's cursor and filter.
    pub fn rescan(&self, scan: Scan, tools: &[Tool]) -> Self {
        let at = self
            .editor
            .cursor_entry()
            .map(|id| self.worktrees[id.0].path.clone());
        let mut next = Self::new(scan, tools);
        let entry = at
            .and_then(|path| next.worktrees.iter().position(|w| w.path == path))
            .map(EntryId);
        next.editor.restore_view(entry, self.editor.filter());
        next
    }

    pub fn shows(&self, scan: &Scan) -> bool {
        *self.worktrees == *scan.worktrees
    }

    /// Re-reads every status and herdr's state in the background, keeping the old ones on
    /// screen until the new ones arrive.
    pub fn refresh(&mut self) {
        self.inspections = inspect_all(&self.worktrees, discover::WORKERS);
        self.herdr_updates = read_herdr();
    }

    pub fn is_loading(&self) -> bool {
        self.loading > 0
    }

    pub fn receive(&mut self) {
        for (index, result) in self.inspections.try_iter() {
            if self.results[index].replace(result).is_none() {
                self.loading -= 1;
            }
        }
        if let Ok(state) = self.herdr_updates.try_recv() {
            self.herdr = Some(state);
        }
    }

    pub fn state(&self, id: EntryId) -> String {
        format::state(&self.worktrees[id.0], self.results[id.0].as_ref())
    }

    /// Removable worktrees known to lose nothing, and how many are still being checked.
    pub fn safe_entries(&self) -> (HashSet<EntryId>, usize) {
        let safe = self
            .results
            .iter()
            .enumerate()
            .filter(|(index, _)| !self.worktrees[*index].main)
            .filter(|(_, r)| matches!(r, Some(Ok(i)) if i.tier == Tier::Safe))
            .map(|(index, _)| EntryId(index))
            .collect();
        (safe, self.loading)
    }

    /// Tiers known so far, by entry.
    pub fn tiers(&self) -> Vec<Option<Tier>> {
        self.results
            .iter()
            .map(|r| r.as_ref().and_then(|r| r.as_ref().ok()).map(|i| i.tier))
            .collect()
    }

    fn rows(&self) -> Vec<Row<'_>> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
        self.editor
            .rows()
            .map(|(_, line)| match line {
                BufferLine::Existing { id, text } => {
                    let worktree = &self.worktrees[id.0];
                    let result = self.results[id.0].as_ref();
                    let editable = !worktree.main && matches!(worktree.head, Some(Head::Branch(_)));
                    let (name, rest) = plan::existing_columns(text, editable);
                    let (branch, renamed_branch) = match &worktree.head {
                        Some(Head::Branch(old)) if editable => (rest.to_owned(), rest != old),
                        head => (format::branch(head.as_ref()), false),
                    };
                    Row {
                        repo: Some(&worktree.repo),
                        mark: if worktree.main {
                            Span::raw("")
                        } else {
                            mark(result)
                        },
                        text,
                        name,
                        branch_text: editable.then_some(rest),
                        branch,
                        branch_style: Style::new().fg(if renamed_branch {
                            theme::YELLOW
                        } else {
                            theme::TAN
                        }),
                        hint: None,
                        state: format::state(worktree, result),
                        age: age(result, now),
                        source: format::source(worktree),
                        herdr: self.herdr_marker(worktree),
                        style: if name == entry_name(worktree) {
                            Style::new()
                        } else {
                            Style::new().fg(theme::YELLOW)
                        },
                        state_style: match (worktree.health, result) {
                            (Health::Prunable | Health::Orphaned, _) | (_, Some(Err(_))) => {
                                Style::new().fg(theme::RED)
                            }
                            _ => Style::new().fg(theme::GRAY),
                        },
                    }
                }
                BufferLine::New { text, near } => {
                    let (name, rest) = plan::columns(text);
                    Row {
                        // The hint names any other repo, so the line stays where it was typed.
                        repo: near.map(|id| self.worktrees[id.0].repo.as_path()),
                        mark: Span::raw("+").fg(theme::GREEN),
                        text,
                        name,
                        branch_text: Some(rest),
                        branch: rest.to_owned(),
                        branch_style: Style::new().fg(theme::GREEN),
                        hint: Some(new_line_hint(text, near.is_some())),
                        state: String::new(),
                        age: Span::raw(""),
                        source: "",
                        herdr: Span::raw(""),
                        style: Style::new().fg(theme::GREEN),
                        state_style: Style::new(),
                    }
                }
            })
            .collect()
    }

    fn herdr_marker(&self, worktree: &Worktree) -> Span<'static> {
        match self
            .herdr
            .as_ref()
            .and_then(|state| state.workspace_for(&worktree.path))
        {
            Some(workspace) if workspace.agents > 0 => {
                let (icon, label, color) = match workspace.status {
                    AgentStatus::Working => ("●", "working", theme::YELLOW),
                    AgentStatus::Blocked => ("▲", "blocked", theme::ORANGE),
                    AgentStatus::Done => ("✓", "done", theme::GREEN),
                    AgentStatus::Idle => ("◆", "idle", theme::GRAY),
                    AgentStatus::Unknown => ("◆", "agent", theme::GRAY),
                };
                let count = match workspace.agents {
                    1 => String::new(),
                    n => format!(" ×{n}"),
                };
                Span::raw(format!("{icon} {label}{count}")).fg(color)
            }
            Some(_) => Span::raw("◇ open").fg(theme::GRAY),
            None => Span::raw(""),
        }
    }

    /// `focused` is false while a pop-up sits on top, which then owns the cursor.
    /// `pick` shows the keys that choose worktrees for an agent.
    pub fn render(&mut self, frame: &mut Frame, focused: bool, pick: bool) {
        let area = super::framed(frame, super::screen(frame), "jumar", self.summary());
        let [header, list, _, status] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(area);
        let status = super::footer_area(status);
        let rows = self.rows();
        let widths = fit(column_widths(&rows), usize::from(list.width));
        let hint_room =
            usize::from(list.width).saturating_sub(width(INDENT) + MARK_WIDTH + width(ARROW));
        let display = group(&rows, hint_room);
        let (row, col) = self.editor.cursor();
        let cursor_pos = self.editor.rows().position(|(index, _)| index == row);
        let cursor_line = cursor_pos.and_then(|pos| {
            display
                .iter()
                .position(|d| matches!(d, Display::Row(p) if *p == pos))
        });
        let height = usize::from(list.height);
        let scroll = cursor_line.map_or(self.scroll, |line| {
            scroll_to(&display, line, self.scroll, height)
        });
        let drawn = focused && !matches!(self.editor.mode(), Mode::Command | Mode::Filter);
        let cells = cursor_pos
            .filter(|_| drawn)
            .map(|pos| cursor_cells(&rows[pos], col, widths));

        let selected: HashSet<usize> = self.selected().collect();
        let titles = HEADERS.map(|h| (h.to_owned(), Style::new().fg(theme::GRAY).bold()));
        let mut header_widths = widths;
        header_widths[0] += MARK_WIDTH;
        frame.render_widget(columns(None, titles, header_widths), header);
        let lines: Vec<Line> = display
            .iter()
            .enumerate()
            .skip(scroll)
            .take(height)
            .map(|(index, item)| match item {
                Display::Gap => Line::default(),
                Display::Group(repo) => group_line(repo, &self.home, list.width),
                Display::Hint { text, style, first } => hint_line(text, *style, *first),
                Display::Row(pos) => {
                    let cursor = cells.as_ref().filter(|_| Some(index) == cursor_line);
                    let line = row_line(&rows[*pos], widths, cursor);
                    if selected.contains(pos) {
                        line.bg(theme::BG2)
                    } else if Some(index) == cursor_line {
                        line.bg(theme::BG1)
                    } else {
                        line
                    }
                }
            })
            .collect();
        if rows.is_empty() {
            let empty = if self.editor.filter().is_empty() {
                "no repos found (o, then ~/path/to/repo:branch to create a worktree)"
            } else {
                "no worktrees match the filter"
            };
            let text = vec![Line::default(), Line::from(empty)];
            frame.render_widget(Paragraph::new(text).fg(theme::GRAY), list);
        } else {
            frame.render_widget(Paragraph::new(lines), list);
        }

        let status_line = self.status_line();
        let prompt_end = status_line.width();
        frame.render_widget(Paragraph::new(status_line), status);
        let keys: &[(&str, &str)] = if pick {
            &[("⏎", "work here"), ("⇥", "add as context"), ("?", "help")]
        } else {
            &[("?", "help")]
        };
        frame.render_widget(
            Line::from(hints(keys.iter().copied())).right_aligned(),
            status,
        );

        match self.editor.mode() {
            _ if !focused => {}
            Mode::Command | Mode::Filter => {
                frame.set_cursor_position((status.x + to_u16(prompt_end), status.y));
            }
            Mode::Normal | Mode::Visual | Mode::Insert => {
                // A list too short to show the cursor's line, such as a 0-row pane while
                // a split opens, leaves the cursor hidden.
                let y = cursor_line.and_then(|line| line.checked_sub(scroll));
                if let (Some((_, _, x)), Some(y)) = (&cells, y.filter(|y| *y < height)) {
                    let x = width(INDENT) + MARK_WIDTH + x;
                    frame.set_cursor_position((list.x + to_u16(x), list.y + to_u16(y)));
                }
            }
        }
        drop(rows);
        self.scroll = scroll;
    }

    /// Mode tag followed by the prompt, filter or message. The cursor sits at its end in
    /// command and filter mode.
    fn status_line(&self) -> Line<'_> {
        let (tag, color) = match self.editor.mode() {
            Mode::Normal => ("NORMAL", theme::YELLOW),
            Mode::Visual => ("VISUAL", theme::ORANGE),
            Mode::Insert => ("INSERT", theme::GREEN),
            Mode::Command => ("COMMAND", theme::YELLOW),
            Mode::Filter => ("FILTER", theme::AQUA),
        };
        let mut spans = vec![mode_tag(tag, color)];
        match self.editor.mode() {
            Mode::Command => spans.push(Span::raw(format!(" :{}", self.editor.prompt()))),
            Mode::Filter => spans.push(Span::raw(format!(" /{}", self.editor.filter()))),
            Mode::Visual => {
                let selected = self.selected().count();
                spans.push(Span::raw(format!(" {selected} selected")));
                spans.extend(hints([("d", "delete")]));
            }
            Mode::Normal | Mode::Insert => {
                if self.editor.is_leader_pending() {
                    spans.extend(hints(
                        self.tool_hint.iter().map(|(k, n)| (k.as_str(), n.as_str())),
                    ));
                } else if let Some(message) = self.editor.message() {
                    spans.push(Span::raw(format!(" {message}")));
                } else if !self.editor.filter().is_empty() {
                    spans.push(Span::raw(format!(" /{}", self.editor.filter())).fg(theme::GRAY));
                }
            }
        }
        Line::from(spans)
    }

    /// Positions among the visible rows of the visual selection.
    fn selected(&self) -> impl Iterator<Item = usize> {
        let range = self.editor.selection();
        self.editor
            .rows()
            .enumerate()
            .filter(move |(_, (index, _))| {
                range.is_some_and(|(first, last)| (first..=last).contains(index))
            })
            .map(|(pos, _)| pos)
    }

    fn summary(&self) -> Line<'static> {
        let mut parts = Vec::new();
        if self.loading > 0 {
            parts.push(format!("loading {}", self.loading));
        }
        parts.push(format!("{} worktrees", self.linked));
        if self.editor.is_modified() {
            parts.push("[+]".into());
        }
        Line::from(format!(" {} ", parts.join("  "))).fg(theme::GRAY)
    }
}

fn read_herdr() -> Receiver<herdr::State> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        if let Ok(state) = Herdr::new().state() {
            // The buffer may already be gone after a reload.
            let _ = tx.send(state);
        }
    });
    rx
}

/// Lays rows out under a title for each repo, in the order they come, with hints wrapped
/// to `hint_room` columns.
fn group<'a>(rows: &[Row<'a>], hint_room: usize) -> Vec<Display<'a>> {
    let mut display = Vec::new();
    let mut current = None;
    for (pos, row) in rows.iter().enumerate() {
        if let Some(repo) = row.repo
            && current != Some(repo)
        {
            if current.is_some() {
                display.push(Display::Gap);
            }
            display.push(Display::Group(repo));
            current = Some(repo);
        }
        display.push(Display::Row(pos));
        if let Some(hint) = &row.hint {
            for (index, text) in wrap(&hint.content, hint_room).into_iter().enumerate() {
                display.push(Display::Hint {
                    text,
                    style: hint.style,
                    first: index == 0,
                });
            }
        }
    }
    display
}

fn column_widths(rows: &[Row]) -> [usize; 6] {
    [
        column_width(rows.iter().map(|r| r.name).chain([HEADERS[0]])),
        column_width(rows.iter().map(|r| r.branch.as_str()).chain([HEADERS[1]])),
        column_width(rows.iter().map(|r| r.state.as_str()).chain([HEADERS[2]])),
        column_width(
            rows.iter()
                .map(|r| r.age.content.as_ref())
                .chain([HEADERS[3]]),
        ),
        column_width(rows.iter().map(|r| r.source).chain([HEADERS[4]])),
        column_width(
            rows.iter()
                .map(|r| r.herdr.content.as_ref())
                .chain([HEADERS[5]]),
        ),
    ]
}

/// Fits the columns in `available`: hides SOURCE, then AGE, then narrows the wider of NAME
/// and BRANCH, then STATUS, none below `MIN_CUT`. Past that it hides HERDR, then STATUS,
/// then narrows NAME and BRANCH to a column each. A hidden column has width 0.
fn fit(mut widths: [usize; 6], available: usize) -> [usize; 6] {
    let total = |widths: &[usize; 6]| {
        let shown = widths.iter().filter(|w| **w > 0).count();
        width(INDENT)
            + MARK_WIDTH
            + widths.iter().sum::<usize>()
            + width(GAP) * shown.saturating_sub(1)
    };
    let hide = |widths: &mut [usize; 6], columns: [usize; 2]| {
        for column in columns {
            if total(widths) > available {
                widths[column] = 0;
            }
        }
    };
    let narrow = |widths: &mut [usize; 6], min: usize| {
        while total(widths) > available {
            let wider = usize::from(widths[1] > widths[0]);
            let Some(column) = [wider, 1 - wider, 2].into_iter().find(|&c| widths[c] > min) else {
                break;
            };
            widths[column] -= 1;
        }
    };
    hide(&mut widths, [4, 3]);
    narrow(&mut widths, MIN_CUT);
    hide(&mut widths, [5, 2]);
    narrow(&mut widths, 1);
    widths
}

fn age(result: Option<&InspectResult>, now: i64) -> Span<'static> {
    match result
        .and_then(|r| r.as_ref().ok())
        .and_then(|i| i.last_commit)
    {
        Some(at) => {
            let seconds = (now - at).max(0);
            let color = if seconds >= STALE_SECONDS {
                theme::YELLOW
            } else {
                theme::GRAY
            };
            Span::raw(format::age(seconds)).fg(color)
        }
        None => Span::raw(""),
    }
}

/// `cursor` holds the name and branch cells from `cursor_cells` when the cursor is drawn
/// on this line.
fn row_line(
    row: &Row,
    widths: [usize; 6],
    cursor: Option<&(String, String, usize)>,
) -> Line<'static> {
    let (name, branch) = match cursor {
        Some((name, branch, _)) => (name.clone(), branch.clone()),
        None => (
            truncate(row.name, widths[0]),
            truncate(&row.branch, widths[1]),
        ),
    };
    let cells = [
        (name, row.style.add_modifier(Modifier::BOLD)),
        (branch, row.branch_style),
        (truncate(&row.state, widths[2]), row.state_style),
        (row.age.content.to_string(), row.age.style),
        (row.source.to_owned(), Style::new().fg(theme::GRAY)),
        (row.herdr.content.to_string(), row.herdr.style),
    ];
    columns(Some(&row.mark), cells, widths)
}

/// Skips hidden columns and leaves the last one unpadded. Without a mark, the first cell
/// starts in its slot.
fn columns(
    mark: Option<&Span<'static>>,
    cells: [(String, Style); 6],
    widths: [usize; 6],
) -> Line<'static> {
    let mut spans = vec![Span::raw(INDENT)];
    if let Some(mark) = mark {
        spans.push(Span::raw(pad(&mark.content, MARK_WIDTH)).style(mark.style));
    }
    let first = widths.iter().position(|w| *w > 0);
    let last = widths.iter().rposition(|w| *w > 0);
    for (index, (text, style)) in cells.into_iter().enumerate() {
        if widths[index] == 0 {
            continue;
        }
        if Some(index) != first {
            spans.push(Span::raw(GAP));
        }
        let text = if Some(index) == last {
            text
        } else {
            pad(&text, widths[index])
        };
        spans.push(Span::styled(text, style));
    }
    Line::from(spans)
}

/// The cursor line's name and branch cells, cut to their columns but with the cursor kept
/// in view, and the cursor's screen column. The cursor, at char `col` of `text`, sits
/// within the directory `name`, the gap after it, or the branch. Both are trimmed parts of
/// `text`; a line without a branch to edit is all directory.
fn cursor_cells(row: &Row, col: usize, widths: [usize; 6]) -> (String, String, usize) {
    let text = row.text;
    let chars: Vec<char> = text.chars().collect();
    let col_of = |byte: usize| text[..byte].chars().count();
    let lead = col_of(text.len() - text.trim_start().len());
    let dir_end = lead + row.name.chars().count();
    let branch_x = widths[0] + width(GAP);
    // Past the directory, it stays cut to its end, so the text just typed stays in view.
    let at_dir = || {
        let (name, x) = window(row.name, col.saturating_sub(lead), widths[0]);
        let past = col.min(chars.len()).saturating_sub(dir_end);
        let x = (x + past).min(branch_x - 1);
        (name, truncate(&row.branch, widths[1]), x)
    };
    let Some(branch) = row.branch_text else {
        return at_dir();
    };
    let rest = if branch.is_empty() {
        chars.len()
    } else {
        col_of(text.trim_end().len() - branch.len())
    };
    if col < rest {
        at_dir()
    } else {
        let (cell, x) = window(branch, col - rest, widths[1]);
        let past = col
            .min(chars.len())
            .saturating_sub(rest + branch.chars().count());
        (truncate(row.name, widths[0]), cell, branch_x + x + past)
    }
}

/// The scroll that keeps display `line` in view, with its group title above and any
/// hint below.
fn scroll_to(display: &[Display], line: usize, scroll: usize, height: usize) -> usize {
    // Scrolling up to a group's first row shows its title too.
    let top = match line.checked_sub(1).map(|l| &display[l]) {
        Some(Display::Group(_)) => line - 1,
        _ => line,
    };
    // A new line's hint scrolls into view with it.
    let bottom = line
        + display[line + 1..]
            .iter()
            .take_while(|d| matches!(d, Display::Hint { .. }))
            .count();
    scroll.min(top).max((bottom + 1).saturating_sub(height))
}

/// The repo's name and path, cut to `room` columns.
fn group_line(repo: &Path, home: &Path, room: u16) -> Line<'static> {
    const ICON: &str = "\u{f418} ";
    let room = usize::from(room).saturating_sub(width(ICON));
    let name = truncate(&format::file_name(repo), room);
    let path_room = room.saturating_sub(width(&name) + 2);
    let path = truncate(&format::tilde(repo, home), path_room);
    let gap = if path.is_empty() { "" } else { "  " };
    Line::from(vec![
        Span::raw(ICON).fg(theme::YELLOW),
        Span::raw(name).fg(theme::YELLOW).bold(),
        Span::raw(gap),
        Span::raw(path).fg(theme::GRAY),
    ])
}

fn hint_line(text: &str, style: Style, first: bool) -> Line<'static> {
    let arrow = if first { ARROW } else { "  " };
    Line::from(vec![
        Span::raw(" ".repeat(width(INDENT) + MARK_WIDTH)),
        Span::raw(arrow).fg(theme::GRAY),
        Span::raw(text.to_owned()).style(style),
    ])
}

/// Breaks `text` into lines of at most `room` columns: between ` · ` items where it can,
/// then between words, then inside a word too long for a line.
fn wrap(text: &str, room: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for item in text.split(" · ") {
        match lines.last_mut() {
            Some(last) if width(last) + width(" · ") + width(item) <= room => {
                last.push_str(" · ");
                last.push_str(item);
            }
            _ => lines.extend(wrap_words(item, room)),
        }
    }
    lines
}

fn wrap_words(text: &str, room: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split(' ') {
        if !line.is_empty() {
            if width(&line) + 1 + width(word) <= room {
                line.push(' ');
            } else {
                lines.push(std::mem::take(&mut line));
            }
        }
        for c in word.chars() {
            if !line.is_empty() && width(&format!("{line}{c}")) > room {
                lines.push(std::mem::take(&mut line));
            }
            line.push(c);
        }
    }
    lines.push(line);
    lines
}

/// What a new line will create, or why it can't.
/// `in_repo` is whether the line has a repo to fall back on when it names none.
fn new_line_hint(text: &str, in_repo: bool) -> Span<'static> {
    if text.trim().is_empty() {
        let help = if in_repo {
            "branch · dir branch · branch@base · #123 · repo:branch"
        } else {
            "type a repo's path, then : and a branch, like ~/code/app:feat"
        };
        return Span::raw(help).fg(theme::GRAY);
    }
    match plan::parse_target(text) {
        Ok(target) if target.repo.is_none() && !in_repo => {
            Span::raw("write the repo's path first, like ~/code/app:feat").fg(theme::RED)
        }
        Ok(target) => {
            let start = match (target.pull, &target.base) {
                (Some(n), _) => Some(format!("from PR #{n}")),
                (None, Some(base)) => Some(format!("from {base}")),
                (None, None) => None,
            };
            let parts = [
                Some(format!("dir {}", target.dir)),
                Some(format!("branch {}", target.name)),
                start,
                target.repo.map(|repo| format!("in {repo}")),
            ];
            Span::raw(parts.into_iter().flatten().collect::<Vec<_>>().join(" · ")).fg(theme::GRAY)
        }
        Err(err) => Span::raw(err.to_string()).fg(theme::RED),
    }
}

/// The text a row starts with: its directory, then its branch when it can be renamed.
fn entry_text(worktree: &Worktree) -> String {
    match &worktree.head {
        Some(Head::Branch(branch)) if !worktree.main => {
            format!("{} {branch}", entry_name(worktree))
        }
        _ => entry_name(worktree).to_owned(),
    }
}

/// The directory a row starts with; any other is a move.
fn entry_name(worktree: &Worktree) -> &str {
    if worktree.main {
        "(main)"
    } else {
        worktree
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
    }
}

/// The removal-safety icon; main checkouts, which cannot be removed, get none.
fn mark(result: Option<&InspectResult>) -> Span<'static> {
    let color = match result {
        None => theme::GRAY,
        Some(Err(_)) => theme::RED,
        Some(Ok(inspection)) => super::tier_color(inspection.tier),
    };
    Span::raw(format::tier_mark(result)).fg(color)
}

#[cfg(test)]
mod tests {
    use jumar_core::editor::Key;
    use jumar_core::model::Head;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    fn buffer() -> Buffer {
        let worktree = Worktree {
            path: PathBuf::from("/nonexistent/app/.claude/worktrees/feat"),
            repo: PathBuf::from("/nonexistent/app"),
            source: Some("claude".into()),
            head: Some(Head::Branch("feat".into())),
            health: Health::Healthy,
            locked: None,
            main: false,
            default_branch: None,
        };
        let scan = Scan {
            worktrees: vec![worktree],
            warnings: Vec::new(),
        };
        Buffer::new(scan, &[])
    }

    #[test]
    fn render_should_not_panic_in_tiny_terminals() {
        let mut buffer = buffer();
        for width in 0..12 {
            for height in 0..12 {
                let mut terminal = Terminal::new(TestBackend::new(width, height))
                    .unwrap_or_else(|err| panic!("no terminal at {width}x{height}: {err}"));
                let drawn = terminal.draw(|frame| buffer.render(frame, true, false));
                assert!(drawn.is_ok(), "draw failed at {width}x{height}");
            }
        }
    }

    #[test]
    fn render_should_cut_long_names_to_fit_the_frame() {
        let mut worktree = buffer().worktrees[0].clone();
        worktree.head = Some(Head::Branch(format!("feat/{}", "x".repeat(80))));
        let mut other = worktree.clone();
        other.path = PathBuf::from(format!(
            "/nonexistent/app/.claude/worktrees/{}",
            "d".repeat(40)
        ));
        let scan = Scan {
            worktrees: vec![worktree, other],
            warnings: Vec::new(),
        };
        let mut buffer = Buffer::new(scan, &[]);
        let mut terminal = Terminal::new(TestBackend::new(60, 10)).expect("terminal");
        terminal
            .draw(|frame| buffer.render(frame, true, false))
            .expect("draw");
        let screen = terminal.backend().buffer();
        let line = |y: u16| -> String { (0..60).map(|x| screen[(x, y)].symbol()).collect() };

        assert!(line(2).contains("HERDR"), "header cut: {}", line(2));
        // The cursor sits on the first line, at the start of its name.
        assert!(
            line(4).contains("feat              feat/xxxxxxxxx…"),
            "{}",
            line(4)
        );
        assert!(line(5).contains("d…   feat/xxxxxxxxx…"), "{}", line(5));
    }

    #[test]
    fn fit_should_leave_columns_that_fit() {
        assert_eq!(fit([4, 6, 6, 3, 6, 5], 80), [4, 6, 6, 3, 6, 5]);
    }

    #[test]
    fn fit_should_hide_source_then_age_before_cutting() {
        // Indent, mark and gaps take 19 columns with all six shown, 16 with five.
        assert_eq!(fit([4, 6, 6, 3, 6, 5], 48), [4, 6, 6, 3, 0, 5]);
        assert_eq!(fit([4, 6, 6, 3, 6, 5], 39), [4, 6, 6, 0, 0, 5]);
    }

    #[test]
    fn fit_should_cut_the_wider_of_name_and_branch_first() {
        assert_eq!(fit([10, 40, 6, 3, 6, 5], 48), [10, 14, 6, 0, 0, 5]);
        assert_eq!(fit([20, 20, 6, 3, 6, 5], 41), [8, 9, 6, 0, 0, 5]);
    }

    #[test]
    fn fit_should_hide_herdr_then_status_past_the_shortest_cut() {
        assert_eq!(fit([8, 8, 8, 3, 6, 5], 37), [8, 8, 8, 0, 0, 0]);
        assert_eq!(fit([8, 8, 8, 3, 6, 5], 30), [8, 8, 0, 0, 0, 0]);
        assert_eq!(fit([8, 8, 8, 3, 6, 5], 12), [2, 3, 0, 0, 0, 0]);
    }

    #[test]
    fn fit_should_cut_status_last() {
        assert_eq!(fit([8, 8, 30, 3, 6, 5], 45), [8, 8, 11, 0, 0, 5]);
    }

    fn row<'a>(text: &'a str, name: &'a str, branch: Option<&'a str>) -> Row<'a> {
        Row {
            repo: None,
            mark: Span::raw(""),
            text,
            name,
            branch_text: branch,
            branch: branch.unwrap_or("-").to_owned(),
            branch_style: Style::new(),
            hint: None,
            state: String::new(),
            age: Span::raw(""),
            source: "",
            herdr: Span::raw(""),
            style: Style::new(),
            state_style: Style::new(),
        }
    }

    fn cursor_x(
        text: &str,
        name: &str,
        branch: Option<&str>,
        col: usize,
        widths: [usize; 2],
    ) -> usize {
        let row = row(text, name, branch);
        cursor_cells(&row, col, [widths[0], widths[1], 6, 3, 6, 5]).2
    }

    #[test]
    fn cursor_x_should_follow_the_text_into_the_branch_column() {
        let xs =
            [0, 3, 4, 10].map(|col| cursor_x("tui feat/x", "tui", Some("feat/x"), col, [5, 8]));

        assert_eq!(xs, [0, 3, 8, 14]);
    }

    #[test]
    fn cursor_x_should_jump_to_the_branch_column_after_a_typed_space() {
        assert_eq!(cursor_x("tui ", "tui", Some(""), 4, [5, 8]), 8);
    }

    #[test]
    fn cursor_x_should_put_a_spaced_directory_before_the_last_word() {
        let text = "my dir feat";
        let xs = [5, 7].map(|col| cursor_x(text, "my dir", Some("feat"), col, [6, 8]));

        assert_eq!(xs, [5, 9]);
    }

    #[test]
    fn cursor_x_should_keep_a_line_without_a_branch_in_its_directory() {
        assert_eq!(cursor_x("my dir ", "my dir", None, 7, [6, 8]), 7);
    }

    #[test]
    fn cursor_cells_should_keep_the_end_of_a_cut_directory_after_a_typed_space() {
        let text = "a-very-long-dir  ";
        let row = row(text, "a-very-long-dir", Some(""));
        let cells = [15, 16].map(|col| cursor_cells(&row, col, [8, 10, 6, 3, 6, 5]));

        assert_eq!(cells[0].0, "…ng-dir");
        assert_eq!(cells[1].0, "…ng-dir");
        assert_eq!([cells[0].2, cells[1].2], [7, 8]);
    }

    #[test]
    fn cursor_x_should_keep_a_cut_directory_out_of_the_branch_column() {
        assert_eq!(
            cursor_x("a-very-long-dir      ", "a-very-long-dir", None, 21, [8, 8]),
            10
        );
    }

    #[test]
    fn cursor_cells_should_cut_the_branch_being_typed_from_the_start() {
        let text = "tui feat/a-long-branch";
        let row = row(text, "tui", Some("feat/a-long-branch"));
        let cells = cursor_cells(&row, text.chars().count(), [5, 10, 6, 3, 6, 5]);

        assert_eq!(cells, ("tui".into(), "…g-branch".into(), 17));
    }

    #[test]
    fn render_should_wrap_a_new_line_hint_to_fit_the_frame() {
        let mut buffer = buffer();
        buffer.editor.handle(Key::Char('o'));
        let mut terminal = Terminal::new(TestBackend::new(32, 12)).expect("terminal");
        terminal
            .draw(|frame| buffer.render(frame, true, false))
            .expect("draw");
        let screen = terminal.backend().buffer();
        let line = |y: u16| -> String { (0..32).map(|x| screen[(x, y)].symbol()).collect() };
        let shown: Vec<String> = (0..12).map(line).collect();

        let hint = shown
            .iter()
            .position(|l| l.contains('↳'))
            .unwrap_or_else(|| panic!("no hint in {shown:#?}"));
        assert!(shown[hint].contains("branch · dir branch"), "{shown:#?}");
        assert!(shown[hint + 1].contains("branch@base · #123"), "{shown:#?}");
        assert!(shown[hint + 2].contains("repo:branch"), "{shown:#?}");
    }

    #[test]
    fn wrap_should_break_between_items_then_words_then_inside_a_word() {
        assert_eq!(wrap("ab · cd · ef", 7), ["ab · cd", "ef"]);
        assert_eq!(
            wrap("write a base after it", 10),
            ["write a", "base after", "it"]
        );
        assert_eq!(wrap("x feat/abcdefgh", 6), ["x", "feat/a", "bcdefg", "h"]);
        assert_eq!(
            wrap("anything", 0),
            ["a", "n", "y", "t", "h", "i", "n", "g"]
        );
    }

    #[test]
    fn scroll_to_should_keep_every_line_of_a_wrapped_hint_in_view() {
        let hint = || Display::Hint {
            text: String::new(),
            style: Style::new(),
            first: false,
        };
        let display = [Display::Row(0), Display::Row(1), hint(), hint(), hint()];

        assert_eq!(scroll_to(&display, 1, 0, 3), 2);
    }

    #[test]
    fn new_line_hint_should_say_what_the_line_creates() {
        let hint = new_line_hint("api:tui feat/x@main", true);

        assert_eq!(hint.content, "dir tui · branch feat/x · from main · in api");
    }

    #[test]
    fn new_line_hint_should_say_why_a_line_cannot_be_read() {
        let hint = new_line_hint("fix@", true);

        assert_eq!(hint.content, "write a base after `@`, or leave out the `@`");
    }

    #[test]
    fn new_line_hint_should_ask_for_a_repo_path_when_no_repo_is_listed() {
        let hints = ["", "feat", "~/code/app:feat"].map(|text| new_line_hint(text, false).content);

        assert_eq!(
            hints.each_ref().map(AsRef::as_ref),
            [
                "type a repo's path, then : and a branch, like ~/code/app:feat",
                "write the repo's path first, like ~/code/app:feat",
                "dir feat · branch feat · in ~/code/app",
            ]
        );
    }

    #[test]
    fn entry_text_should_hold_the_branch_of_a_linked_worktree_only() {
        let mut worktree = buffer().worktrees[0].clone();
        let linked = entry_text(&worktree);
        worktree.main = true;

        assert_eq!(
            (linked.as_str(), entry_text(&worktree).as_str()),
            ("feat feat", "(main)")
        );
    }
}
