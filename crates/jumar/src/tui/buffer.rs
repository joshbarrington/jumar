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

use super::{column_width, hints, mode_tag, pad, theme, to_u16, truncate, width};
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
    Hint(usize),
}

const HEADERS: [&str; 6] = ["NAME", "BRANCH", "STATUS", "AGE", "SOURCE", "HERDR"];
/// Worktrees whose last commit is older than this show their age in yellow.
const STALE_SECONDS: i64 = 30 * 24 * 60 * 60;
const GAP: &str = "   ";
/// Rows sit under their group's title; the mark column comes first.
const INDENT: &str = "  ";
const MARK_WIDTH: usize = 2;

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
        let widths = fit_branch(column_widths(&rows), &rows, usize::from(list.width));
        let display = group(&rows);
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
                Display::Group(repo) => group_line(repo, &self.home),
                Display::Hint(pos) => hint_line(&rows[*pos]),
                Display::Row(pos) => {
                    let typing = self.editor.mode() == Mode::Insert && Some(index) == cursor_line;
                    let line = row_line(&rows[*pos], widths, typing);
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
                if let (Some(pos), Some(y)) = (cursor_pos, y.filter(|y| *y < height)) {
                    let x = width(INDENT)
                        + MARK_WIDTH
                        + cursor_x(
                            rows[pos].text,
                            rows[pos].name,
                            rows[pos].branch_text,
                            col,
                            widths[0],
                        );
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

/// Lays rows out under a title for each repo, in the order they come.
fn group<'a>(rows: &[Row<'a>]) -> Vec<Display<'a>> {
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
        if row.hint.is_some() {
            display.push(Display::Hint(pos));
        }
    }
    display
}

/// The width of each column but the last, which is left unpadded.
fn column_widths(rows: &[Row]) -> [usize; 5] {
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
    ]
}

/// Narrows the branch column so every column fits in `available`, down to its header.
fn fit_branch(mut widths: [usize; 5], rows: &[Row], available: usize) -> [usize; 5] {
    let herdr = column_width(
        rows.iter()
            .map(|r| r.herdr.content.as_ref())
            .chain([HEADERS[5]]),
    );
    let total = width(INDENT) + MARK_WIDTH + widths.iter().sum::<usize>() + 5 * width(GAP) + herdr;
    let over = total.saturating_sub(available);
    widths[1] = widths[1].saturating_sub(over).max(width(HEADERS[1]));
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

/// `whole` keeps a branch wider than its column instead of cutting it, so the line being
/// typed into stays in view.
fn row_line(row: &Row, widths: [usize; 5], whole: bool) -> Line<'static> {
    let branch = if whole {
        row.branch.clone()
    } else {
        truncate(&row.branch, widths[1])
    };
    let cells = [
        (row.name.to_owned(), row.style.add_modifier(Modifier::BOLD)),
        (branch, row.branch_style),
        (row.state.clone(), row.state_style),
        (row.age.content.to_string(), row.age.style),
        (row.source.to_owned(), Style::new().fg(theme::GRAY)),
        (row.herdr.content.to_string(), row.herdr.style),
    ];
    columns(Some(&row.mark), cells, widths)
}

/// The last column is left unpadded. Without a mark, the first cell starts in its slot.
fn columns(
    mark: Option<&Span<'static>>,
    cells: [(String, Style); 6],
    widths: [usize; 5],
) -> Line<'static> {
    let mut spans = vec![Span::raw(INDENT)];
    if let Some(mark) = mark {
        spans.push(Span::raw(pad(&mark.content, MARK_WIDTH)).style(mark.style));
    }
    for (index, (text, style)) in cells.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw(GAP));
        }
        let text = widths
            .get(index)
            .map_or_else(|| text.clone(), |w| pad(&text, *w));
        spans.push(Span::styled(text, style));
    }
    Line::from(spans)
}

/// Where the cursor at char `col` of `text` sits on screen: within its directory `name`,
/// the gap after it, or its `branch`, whose column starts `name_width` plus the gap in.
/// Both are trimmed parts of `text`; a line without a branch to edit is all directory.
fn cursor_x(text: &str, name: &str, branch: Option<&str>, col: usize, name_width: usize) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let col_of = |byte: usize| text[..byte].chars().count();
    let span = |from: usize, to: usize| width(&chars[from..to].iter().collect::<String>());
    let lead = col_of(text.len() - text.trim_start().len());
    let Some(branch) = branch else {
        return span(lead.min(col), col.min(chars.len()));
    };
    let dir_end = lead + name.chars().count();
    let rest = if branch.is_empty() {
        chars.len()
    } else {
        col_of(text.trim_end().len() - branch.len())
    };
    if col <= dir_end {
        span(lead.min(col), col)
    } else if col < rest {
        (span(lead, dir_end) + col - dir_end).min(name_width + width(GAP) - 1)
    } else {
        name_width + width(GAP) + span(rest, col.min(chars.len()))
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
    let bottom = match display.get(line + 1) {
        Some(Display::Hint(_)) => line + 1,
        _ => line,
    };
    scroll.min(top).max((bottom + 1).saturating_sub(height))
}

fn group_line(repo: &Path, home: &Path) -> Line<'static> {
    Line::from(vec![
        Span::raw("\u{f418} ").fg(theme::YELLOW),
        Span::raw(format::file_name(repo)).fg(theme::YELLOW).bold(),
        Span::raw("  "),
        Span::raw(format::tilde(repo, home)).fg(theme::GRAY),
    ])
}

fn hint_line(row: &Row) -> Line<'static> {
    Line::from(vec![
        Span::raw(" ".repeat(width(INDENT) + MARK_WIDTH)),
        Span::raw("↳ ").fg(theme::GRAY),
        row.hint.clone().unwrap_or_default(),
    ])
}

/// What a new line will create, or why it can't.
/// `in_repo` is whether the line has a repo to fall back on when it names none.
fn new_line_hint(text: &str, in_repo: bool) -> Span<'static> {
    if text.trim().is_empty() {
        let help = if in_repo {
            "[dir] branch[@base]  ·  [dir] #pr_number  ·  [dir] branch@#pr_number  ·  repo: in front for another repo"
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
    fn render_should_cut_long_branches_to_fit_the_frame() {
        let mut buffer = buffer();
        let mut worktree = buffer.worktrees[0].clone();
        worktree.head = Some(Head::Branch(format!("feat/{}", "x".repeat(80))));
        let scan = Scan {
            worktrees: vec![worktree],
            warnings: Vec::new(),
        };
        buffer = Buffer::new(scan, &[]);
        let mut terminal = Terminal::new(TestBackend::new(60, 10)).expect("terminal");
        terminal
            .draw(|frame| buffer.render(frame, false, false))
            .expect("draw");
        let screen = terminal.backend().buffer();
        let row: String = (0..60)
            .map(|x| screen[(x, 4)].symbol().to_owned())
            .collect();

        assert!(row.contains('…'), "branch not cut: {row}");
        assert!(row.contains("claude"), "source column pushed off: {row}");
    }

    #[test]
    fn cursor_x_should_follow_the_text_into_the_branch_column() {
        let xs = [0, 3, 4, 10].map(|col| cursor_x("tui feat/x", "tui", Some("feat/x"), col, 5));

        assert_eq!(xs, [0, 3, 8, 14]);
    }

    #[test]
    fn cursor_x_should_jump_to_the_branch_column_after_a_typed_space() {
        assert_eq!(cursor_x("tui ", "tui", Some(""), 4, 5), 8);
    }

    #[test]
    fn cursor_x_should_put_a_spaced_directory_before_the_last_word() {
        let text = "my dir feat";
        let xs = [5, 7].map(|col| cursor_x(text, "my dir", Some("feat"), col, 6));

        assert_eq!(xs, [5, 9]);
    }

    #[test]
    fn cursor_x_should_keep_a_line_without_a_branch_in_its_directory() {
        assert_eq!(cursor_x("my dir ", "my dir", None, 7, 6), 7);
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
