mod buffer;
mod confirm;
mod help;
mod theme;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use jumar_core::apply::{self, Context};
use jumar_core::config::ToolMode;
use jumar_core::editor::{Action, Key, Mode};
use jumar_core::herdr::Herdr;
use jumar_core::model::Health;
use jumar_core::plan::{self, EntryId};
use jumar_core::safety::Tier;
use jumar_core::tool;
use ratatui::crossterm::cursor::SetCursorStyle;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding};
use ratatui::{DefaultTerminal, Frame};

use crate::discover::{self, Env};
use crate::tools;
use buffer::Buffer;
use confirm::{Confirm, Decision, Report};

pub enum Outcome {
    Quit,
    Open(PathBuf),
}

pub fn run() -> Result<Outcome> {
    let env = Env::load()?;
    let buffer = Buffer::new(discover::scan(&env)?, &env.config.tools());
    let mut app = App {
        env,
        buffer,
        screen: Screen::Edit,
    };
    let mut terminal = ratatui::try_init()?;
    let outcome = app.run(&mut terminal);
    ratatui::restore();
    execute!(io::stdout(), SetCursorStyle::DefaultUserShape)?;
    outcome
}

/// What the event loop must do for the app, beyond redrawing.
enum Effect {
    Exit(Outcome),
    Suspend { command: Vec<String>, dir: PathBuf },
}

enum Screen {
    Edit,
    Confirm(Confirm),
    Report(Report),
    Help,
}

struct App {
    env: Env,
    buffer: Buffer,
    screen: Screen,
}

impl App {
    fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<Outcome> {
        let mut insert_shape = None;
        loop {
            self.buffer.receive();
            if let Screen::Report(report) = &mut self.screen {
                report.receive();
            }
            let wants_bar = !matches!(self.buffer.editor.mode(), Mode::Normal | Mode::Visual);
            if insert_shape != Some(wants_bar) {
                insert_shape = Some(wants_bar);
                let shape = if wants_bar {
                    SetCursorStyle::SteadyBar
                } else {
                    SetCursorStyle::SteadyBlock
                };
                execute!(io::stdout(), shape)?;
            }
            let tools = self.env.config.tools();
            let (buffer, screen) = (&mut self.buffer, &self.screen);
            terminal.draw(|frame| {
                buffer.render(frame, matches!(screen, Screen::Edit));
                match screen {
                    Screen::Edit => {}
                    Screen::Confirm(confirm) => confirm.render(frame),
                    Screen::Report(report) => report.render(frame),
                    Screen::Help => help::render(frame, &tools),
                }
            })?;

            let busy = self.buffer.is_loading()
                || matches!(&self.screen, Screen::Report(r) if r.is_running());
            let timeout = if busy {
                Duration::from_millis(30)
            } else {
                Duration::from_secs(60)
            };
            if !event::poll(timeout)? {
                continue;
            }
            if let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
                && let Some(key) = convert(key)
            {
                match self.handle(key)? {
                    Some(Effect::Exit(outcome)) => return Ok(outcome),
                    Some(Effect::Suspend { command, dir }) => {
                        if let Err(err) = suspend(terminal, &command, &dir)? {
                            self.buffer.editor.set_message(format!("{err:#}"));
                        }
                        insert_shape = None;
                    }
                    None => {}
                }
            }
        }
    }

    fn handle(&mut self, key: Key) -> Result<Option<Effect>> {
        match &mut self.screen {
            Screen::Edit => {
                if let Some(action) = self.buffer.editor.handle(key) {
                    return Ok(self.act(action));
                }
            }
            Screen::Confirm(confirm) => match confirm.handle(key) {
                Some(Decision::Run) => {
                    if let Screen::Confirm(confirm) =
                        std::mem::replace(&mut self.screen, Screen::Edit)
                    {
                        self.screen = Screen::Report(confirm.run());
                    }
                }
                Some(Decision::Cancel) => self.screen = Screen::Edit,
                None => {}
            },
            Screen::Report(report) if !report.is_running() => {
                if report.quit_after {
                    return Ok(Some(Effect::Exit(Outcome::Quit)));
                }
                self.buffer = Buffer::new(discover::scan(&self.env)?, &self.env.config.tools());
                self.screen = Screen::Edit;
            }
            Screen::Report(_) => {}
            Screen::Help => {
                if matches!(key, Key::Char('?' | 'q') | Key::Esc | Key::Ctrl('c')) {
                    self.screen = Screen::Edit;
                }
            }
        }
        Ok(None)
    }

    fn act(&mut self, action: Action) -> Option<Effect> {
        let editor = &mut self.buffer.editor;
        match action {
            Action::Quit => return Some(Effect::Exit(Outcome::Quit)),
            Action::Help => self.screen = Screen::Help,
            Action::Open(_) if editor.is_modified() => {
                editor.set_message("unsaved changes (:w to apply, :q! to discard)");
            }
            Action::Open(id) => {
                let worktree = &self.buffer.worktrees[id.0];
                if !Herdr::is_inside() {
                    return Some(Effect::Exit(Outcome::Open(worktree.path.clone())));
                }
                match Herdr::new().open(&worktree.path, &worktree.repo, true) {
                    Ok(()) => return Some(Effect::Exit(Outcome::Quit)),
                    Err(err) => editor.set_message(err.to_string()),
                }
            }
            Action::Tool { key, id } => return self.run_tool(key, id),
            Action::Write { quit } => match plan::plan(&self.buffer.worktrees, editor.lines()) {
                Ok(operations) if operations.is_empty() => {
                    if quit {
                        return Some(Effect::Exit(Outcome::Quit));
                    }
                    editor.set_message("no changes");
                }
                Ok(operations) => {
                    let tiers = self.buffer.tiers();
                    let sources = self.env.config.sources();
                    let herdr = Herdr::new().state().ok();
                    let fallback = if Herdr::is_inside() {
                        "herdr"
                    } else {
                        "claude"
                    };
                    let context = Context {
                        worktrees: &self.buffer.worktrees,
                        tiers: &tiers,
                        sources: &sources,
                        default_source: self.env.config.default_create_source().unwrap_or(fallback),
                        home: &self.env.home,
                        herdr: herdr.as_ref(),
                    };
                    let items = apply::prepare(&context, operations);
                    let worktrees = Arc::clone(&self.buffer.worktrees);
                    self.screen = Screen::Confirm(Confirm::new(worktrees, items, quit));
                }
                Err(errors) => {
                    let more = match errors.len() {
                        1 => String::new(),
                        n => format!(" (+{} more)", n - 1),
                    };
                    editor.set_message(format!("{}{more}", errors[0]));
                }
            },
            Action::Command(command) => {
                editor.set_message(format!("not an editor command: {command}"));
            }
        }
        None
    }
}

impl App {
    fn run_tool(&mut self, key: char, id: EntryId) -> Option<Effect> {
        let editor = &mut self.buffer.editor;
        let tools = self.env.config.tools();
        let Some(tool) = tools.iter().find(|t| t.key == key) else {
            editor.set_message(format!("no tool on <Space>{key}"));
            return None;
        };
        let worktree = &self.buffer.worktrees[id.0];
        if worktree.health != Health::Healthy {
            editor.set_message(format!("{}: the worktree directory is missing", tool.name));
            return None;
        }
        let command = match tool::render(&tool.command, worktree) {
            Ok(command) => command,
            Err(err) => {
                editor.set_message(format!("{}: {err}", tool.name));
                return None;
            }
        };
        let dir = worktree.path.clone();
        let result = match tool.mode {
            ToolMode::Suspend => return Some(Effect::Suspend { command, dir }),
            ToolMode::Detach => tools::detach(&command, &dir).map(|()| None),
            ToolMode::HerdrWorkspace => herdr_workspace(&command, &dir, &worktree.repo)
                .map(|()| Some(Effect::Exit(Outcome::Quit))),
        };
        match result {
            Ok(effect) => {
                editor.set_message(format!("started {}", tool.name));
                effect
            }
            Err(err) => {
                editor.set_message(format!("{}: {err:#}", tool.name));
                None
            }
        }
    }
}

fn herdr_workspace(command: &[String], dir: &Path, repo: &Path) -> Result<()> {
    if !Herdr::is_inside() {
        anyhow::bail!("needs to run inside herdr");
    }
    Herdr::new().open(dir, repo, true)?;
    tools::run_captured(command, dir)
}

/// Hands the terminal to `command` and takes it back afterwards. The inner result is the
/// command's own failure; the outer one a terminal failure.
fn suspend(terminal: &mut DefaultTerminal, command: &[String], dir: &Path) -> Result<Result<()>> {
    execute!(
        io::stdout(),
        LeaveAlternateScreen,
        SetCursorStyle::DefaultUserShape
    )?;
    disable_raw_mode()?;
    let result = tools::run_attached(command, dir);
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    terminal.clear()?;
    Ok(result)
}

fn convert(event: KeyEvent) -> Option<Key> {
    Some(match event.code {
        KeyCode::Char(c) if event.modifiers.contains(KeyModifiers::CONTROL) => Key::Ctrl(c),
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Esc => Key::Esc,
        KeyCode::Enter => Key::Enter,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        _ => return None,
    })
}

/// Each key as a chip followed by what it does, after a mode tag.
fn hints<'a>(keys: impl IntoIterator<Item = (&'a str, &'a str)>) -> Vec<Span<'static>> {
    keys.into_iter()
        .flat_map(|(key, action)| {
            [
                Span::raw("  "),
                key_chip(key),
                Span::raw(format!(" {action}")).fg(theme::TAN),
            ]
        })
        .collect()
}

fn key_chip(key: &str) -> Span<'static> {
    Span::raw(format!(" {key} "))
        .fg(theme::YELLOW)
        .bg(theme::BG2)
        .bold()
}

fn mode_tag(tag: &str, color: Color) -> Span<'static> {
    Span::raw(format!(" {tag} "))
        .fg(theme::BG0)
        .bg(color)
        .bold()
}

/// Draws the rounded frame every screen sits in and returns the padded area inside it.
fn framed(frame: &mut Frame, area: Rect, title: &str, info: Line) -> Rect {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::GRAY))
        .title(Line::from(format!(" {title} ")).bold().fg(theme::YELLOW))
        .title_top(info.right_aligned())
        .padding(Padding::new(2, 2, 1, 0));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    inner
}

/// Dims whatever is already drawn and frames a centred window sized to `lines` and
/// `footer`. Returns the list and footer areas inside it.
fn popup(frame: &mut Frame, title: &str, lines: &[Line], footer: &Line) -> (Rect, Rect) {
    const BORDER_AND_PADDING: (usize, usize) = (6, 2);
    let screen = frame.area();
    frame
        .buffer_mut()
        .set_style(screen, Style::new().add_modifier(Modifier::DIM));
    let content = lines
        .iter()
        .map(Line::width)
        .chain([footer.width()])
        .max()
        .unwrap_or(0);
    // The footer sits below the list after a blank line.
    let width = to_u16(content + BORDER_AND_PADDING.0).min(screen.width.saturating_sub(4));
    let height =
        to_u16(lines.len() + 2 + BORDER_AND_PADDING.1 + 1).min(screen.height.saturating_sub(2));
    let area = screen.centered(Constraint::Length(width), Constraint::Length(height));
    frame.render_widget(Clear, area);
    let inner = framed(frame, area, title, Line::default());
    let [list, _, status] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);
    (list, status)
}

fn tier_color(tier: Tier) -> Color {
    match tier {
        Tier::Safe => theme::GREEN,
        Tier::Unmerged => theme::YELLOW,
        Tier::Dirty => theme::RED,
    }
}

fn width(text: &str) -> usize {
    Span::raw(text).width()
}

fn column_width<'a>(cells: impl Iterator<Item = &'a str>) -> usize {
    cells.map(width).max().unwrap_or(0)
}

fn pad(text: &str, to: usize) -> String {
    format!("{text}{}", " ".repeat(to.saturating_sub(width(text))))
}

fn to_u16(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pad_should_use_display_width() {
        assert_eq!(pad("🟢", 3), "🟢 ");
    }

    #[test]
    fn convert_should_map_control_chars() {
        let event = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);

        assert_eq!(convert(event), Some(Key::Ctrl('r')));
    }
}
