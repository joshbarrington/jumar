use jumar_core::config::Tool;
use ratatui::Frame;
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::{mode_tag, pad, popup, theme, width};

const SECTIONS: [(&str, &[(&str, &str)]); 5] = [
    (
        "Move",
        &[
            ("j k", "down, up (counts work: 3j)"),
            ("gg G", "first, last line (5G goes to line 5)"),
            ("h l w b 0 $", "move within the name"),
            ("/", "filter by name, repo, source or branch"),
        ],
    ),
    (
        "Edit",
        &[
            ("dd", "remove the worktree"),
            (
                "v V",
                "select lines; d removes them, o jumps to the other end",
            ),
            ("cw ciw x i a A I", "rename it"),
            ("o O", "create one: name, repo/name or name@base"),
            ("u Ctrl-r", "undo, redo"),
        ],
    ),
    (
        "Apply",
        &[
            (":w", "review and apply changes"),
            (":wq :q :q!", "apply and quit, quit, discard and quit"),
            (
                "y n f D",
                "in the review: apply, back, force, toggle branch deletion",
            ),
        ],
    ),
    (
        "Go",
        &[(
            "Enter",
            "go to the worktree (its herdr workspace inside herdr)",
        )],
    ),
    ("Help", &[("?", "toggle this help")]),
];

pub fn render(frame: &mut Frame, tools: &[Tool]) {
    let tool_keys: Vec<(String, &str)> = tools
        .iter()
        .map(|tool| (format!("<Space>{}", tool.key), tool.name.as_str()))
        .collect();
    let mut sections: Vec<(&str, Vec<(&str, &str)>)> = SECTIONS
        .iter()
        .map(|(title, keys)| (*title, keys.to_vec()))
        .collect();
    if !tool_keys.is_empty() {
        sections.push((
            "Tools",
            tool_keys.iter().map(|(k, n)| (k.as_str(), *n)).collect(),
        ));
    }
    let key_width = sections
        .iter()
        .flat_map(|(_, keys)| keys.iter().map(|(key, _)| width(key)))
        .max()
        .unwrap_or(0);

    let mut lines = Vec::new();
    for (index, (title, keys)) in sections.iter().enumerate() {
        if index > 0 {
            lines.push(Line::default());
        }
        lines.push(Line::from(Span::raw(*title).fg(theme::BLUE).bold()));
        for (key, action) in keys {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(pad(key, key_width), Style::new().fg(theme::YELLOW)),
                Span::raw("  "),
                Span::raw(*action),
            ]));
        }
    }
    let footer = Line::from(vec![
        mode_tag("HELP", theme::BLUE),
        Span::raw("  ? or Esc to close").fg(theme::GRAY),
    ]);
    let (list, status) = popup(frame, "keys", &lines, &footer);
    frame.render_widget(Paragraph::new(lines), list);
    frame.render_widget(footer, status);
}
