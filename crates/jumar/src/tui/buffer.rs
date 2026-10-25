use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::thread;

use jumar_core::config::Tool;
use jumar_core::editor::{Editor, Entry, Mode};
use jumar_core::herdr::{self, Herdr};
use jumar_core::inspect::inspect_all;
use jumar_core::model::{Health, Worktree};
use jumar_core::plan::Line as BufferLine;
use jumar_core::safety::Tier;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::{column_width, hints, mode_tag, pad, theme, to_u16, width};
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
    name: &'a str,
    branch: String,
    state: String,
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
}

const HEADERS: [&str; 5] = ["NAME", "BRANCH", "STATUS", "SOURCE", "HERDR"];
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
                name: entry_name(w).to_owned(),
                fixed: w.main,
                keywords: format!(
                    "{} {} {}",
                    format::file_name(&w.repo),
                    format::source(w),
                    format::branch(w.head.as_ref()),
                ),
            })
            .collect();
        let (tx, herdr_updates) = mpsc::channel();
        thread::spawn(move || {
            if let Ok(state) = Herdr::new().state() {
                // The buffer may already be gone after a reload.
                let _ = tx.send(state);
            }
        });
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
            herdr_updates,
            home: std::env::home_dir().unwrap_or_default(),
        }
    }

    pub fn is_loading(&self) -> bool {
        self.loading > 0
    }

    pub fn receive(&mut self) {
        for (index, result) in self.inspections.try_iter() {
            self.results[index] = Some(result);
            self.loading -= 1;
        }
        if let Ok(state) = self.herdr_updates.try_recv() {
            self.herdr = Some(state);
        }
    }

    /// Tiers known so far, by entry.
    pub fn tiers(&self) -> Vec<Option<Tier>> {
        self.results
            .iter()
            .map(|r| r.as_ref().and_then(|r| r.as_ref().ok()).map(|i| i.tier))
            .collect()
    }

    fn rows(&self) -> Vec<Row<'_>> {
        self.editor
            .rows()
            .map(|(_, line)| match line {
                BufferLine::Existing { id, name } => {
                    let worktree = &self.worktrees[id.0];
                    let result = self.results[id.0].as_ref();
                    let renamed = name != entry_name(worktree);
                    Row {
                        repo: Some(&worktree.repo),
                        mark: if worktree.main {
                            Span::raw("")
                        } else {
                            mark(result)
                        },
                        name,
                        branch: format::branch(worktree.head.as_ref()),
                        state: format::state(worktree, result),
                        source: format::source(worktree),
                        herdr: self.herdr_marker(worktree),
                        style: if renamed {
                            Style::new().fg(theme::YELLOW)
                        } else {
                            Style::new()
                        },
                        state_style: match (worktree.health, result) {
                            (Health::Prunable | Health::Orphaned, _) | (_, Some(Err(_))) => {
                                Style::new().fg(theme::RED)
                            }
                            _ => Style::new().fg(theme::GRAY),
                        },
                    }
                }
                BufferLine::New(text) => Row {
                    repo: None,
                    mark: Span::raw("+").fg(theme::GREEN),
                    name: text,
                    branch: String::new(),
                    state: String::new(),
                    source: "",
                    herdr: Span::raw(""),
                    style: Style::new().fg(theme::GREEN),
                    state_style: Style::new(),
                },
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
                Span::raw(format!("◆ {} agent(s)", workspace.agents)).fg(theme::YELLOW)
            }
            Some(_) => Span::raw("◇ open").fg(theme::GRAY),
            None => Span::raw(""),
        }
    }

    /// `focused` is false while a pop-up sits on top, which then owns the cursor.
    pub fn render(&mut self, frame: &mut Frame, focused: bool) {
        let area = super::framed(frame, frame.area(), "jumar", self.summary());
        let [header, list, _, status] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(area);
        let rows = self.rows();
        let widths = [
            column_width(rows.iter().map(|r| r.name).chain([HEADERS[0]])),
            column_width(rows.iter().map(|r| r.branch.as_str()).chain([HEADERS[1]])),
            column_width(rows.iter().map(|r| r.state.as_str()).chain([HEADERS[2]])),
            column_width(rows.iter().map(|r| r.source).chain([HEADERS[3]])),
        ];
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
            // Scrolling up to a group's first row shows its title too.
            let top = match line.checked_sub(1).map(|l| &display[l]) {
                Some(Display::Group(_)) => line - 1,
                _ => line,
            };
            self.scroll.min(top).max((line + 1).saturating_sub(height))
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
                Display::Group(repo) => Line::from(vec![
                    Span::raw("\u{f418} ").fg(theme::YELLOW),
                    Span::raw(format::file_name(repo)).fg(theme::YELLOW).bold(),
                    Span::raw("  "),
                    Span::raw(format::tilde(repo, &self.home)).fg(theme::GRAY),
                ]),
                Display::Row(pos) => {
                    let line = row_line(&rows[*pos], widths);
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
                "no worktrees found (o to create one)"
            } else {
                "no worktrees match the filter"
            };
            frame.render_widget(Paragraph::new(empty).fg(theme::GRAY), list);
        } else {
            frame.render_widget(Paragraph::new(lines), list);
        }

        let status_line = self.status_line();
        let prompt_end = status_line.width();
        frame.render_widget(Paragraph::new(status_line), status);
        frame.render_widget(Line::from(hints([("?", "help")])).right_aligned(), status);

        match self.editor.mode() {
            _ if !focused => {}
            Mode::Command | Mode::Filter => {
                frame.set_cursor_position((status.x + to_u16(prompt_end), status.y));
            }
            Mode::Normal | Mode::Visual | Mode::Insert => {
                if let (Some(pos), Some(line)) = (cursor_pos, cursor_line) {
                    let before: String = rows[pos].name.chars().take(col).collect();
                    let x = width(INDENT) + MARK_WIDTH + width(&before);
                    let y = line - scroll;
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
    }
    display
}

fn row_line(row: &Row, widths: [usize; 4]) -> Line<'static> {
    let cells = [
        (row.name.to_owned(), row.style.add_modifier(Modifier::BOLD)),
        (row.branch.clone(), Style::new().fg(theme::TAN)),
        (row.state.clone(), row.state_style),
        (row.source.to_owned(), Style::new().fg(theme::GRAY)),
        (row.herdr.content.to_string(), row.herdr.style),
    ];
    columns(Some(&row.mark), cells, widths)
}

/// The last column is left unpadded. Without a mark, the first cell starts in its slot.
fn columns(
    mark: Option<&Span<'static>>,
    cells: [(String, Style); 5],
    widths: [usize; 4],
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

/// The text a row starts with; any other text is a rename.
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
