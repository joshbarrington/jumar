mod buffer;
mod confirm;

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use jumar_core::apply::{self, Context};
use jumar_core::editor::{Action, Key, Mode};
use jumar_core::plan;
use ratatui::DefaultTerminal;
use ratatui::crossterm::cursor::SetCursorStyle;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::crossterm::execute;
use ratatui::style::{Color, Stylize};
use ratatui::text::Span;

use crate::discover::{self, Env};
use buffer::Buffer;
use confirm::{Confirm, Decision, Report};

pub enum Outcome {
    Quit,
    Open(PathBuf),
}

pub fn run() -> Result<Outcome> {
    let env = Env::load()?;
    let buffer = Buffer::new(discover::scan(&env)?);
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

enum Screen {
    Edit,
    Confirm(Confirm),
    Report(Report),
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
            let wants_bar = self.buffer.editor.mode() != Mode::Normal;
            if insert_shape != Some(wants_bar) {
                insert_shape = Some(wants_bar);
                let shape = if wants_bar {
                    SetCursorStyle::SteadyBar
                } else {
                    SetCursorStyle::SteadyBlock
                };
                execute!(io::stdout(), shape)?;
            }
            let (buffer, screen) = (&mut self.buffer, &self.screen);
            terminal.draw(|frame| match screen {
                Screen::Edit => buffer.render(frame),
                Screen::Confirm(confirm) => confirm.render(frame),
                Screen::Report(report) => report.render(frame),
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
                && let Some(outcome) = self.handle(key)?
            {
                return Ok(outcome);
            }
        }
    }

    fn handle(&mut self, key: Key) -> Result<Option<Outcome>> {
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
                    return Ok(Some(Outcome::Quit));
                }
                self.buffer = Buffer::new(discover::scan(&self.env)?);
                self.screen = Screen::Edit;
            }
            Screen::Report(_) => {}
        }
        Ok(None)
    }

    fn act(&mut self, action: Action) -> Option<Outcome> {
        let editor = &mut self.buffer.editor;
        match action {
            Action::Quit => return Some(Outcome::Quit),
            Action::Open(_) if editor.is_modified() => {
                editor.set_message("unsaved changes (:w to apply, :q! to discard)");
            }
            Action::Open(id) => {
                return Some(Outcome::Open(self.buffer.worktrees[id.0].path.clone()));
            }
            Action::Write { quit } => match plan::plan(&self.buffer.worktrees, editor.lines()) {
                Ok(operations) if operations.is_empty() => {
                    if quit {
                        return Some(Outcome::Quit);
                    }
                    editor.set_message("no changes");
                }
                Ok(operations) => {
                    let tiers = self.buffer.tiers();
                    let sources = self.env.config.sources();
                    let context = Context {
                        worktrees: &self.buffer.worktrees,
                        tiers: &tiers,
                        sources: &sources,
                        default_source: self.env.config.default_create_source(),
                        home: &self.env.home,
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

fn mode_tag(tag: &str, color: Color) -> Span<'static> {
    Span::raw(format!(" {tag} "))
        .fg(Color::Black)
        .bg(color)
        .bold()
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
