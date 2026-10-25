use jumar_core::config::Tool;
use ratatui::Frame;
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::{hints, mode_tag, pad, popup, theme, width};

const SECTIONS: [(&str, &[(&str, &str)]); 4] = [
    (
        "Edit",
        &[
            ("/", "filter by name, repo, source or branch"),
            ("dd", "remove the worktree (v V then d for several)"),
            (":clean", "remove every safe worktree shown"),
            ("cw", "rename it"),
            (
                "o O",
                "create one: name, repo/name, name@base or #123 for a PR",
            ),
        ],
    ),
    (
        "Apply",
        &[
            (":w", "review and apply changes"),
            (
                "y q f D",
                "in the review: apply, back, force, toggle branch deletion",
            ),
        ],
    ),
    (
        "Go",
        &[
            (
                "Enter",
                "go to the worktree (its herdr workspace inside herdr)",
            ),
            ("yp", "copy its path"),
            ("R :e", "reload and refresh status (:e! drops edits)"),
        ],
    ),
    ("Help", &[("?", "toggle this help")]),
];

/// `pick` adds the keys that choose worktrees for an agent.
pub fn render(frame: &mut Frame, tools: &[Tool], pick: bool) {
    let tool_keys: Vec<(String, &str)> = tools
        .iter()
        .map(|tool| (format!("<Space>{}", tool.key), tool.name.as_str()))
        .collect();
    let mut sections: Vec<(&str, Vec<(&str, &str)>)> = SECTIONS
        .iter()
        .map(|(title, keys)| (*title, keys.to_vec()))
        .collect();
    if pick {
        sections.insert(
            0,
            (
                "Pick",
                vec![
                    ("Enter", "have the agent work in the worktree"),
                    (
                        "Tab",
                        "pass it to the agent as context (v V then Tab for several)",
                    ),
                ],
            ),
        );
    }
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
        lines.push(Line::from(Span::raw(*title).fg(theme::YELLOW).bold()));
        for (key, action) in keys {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(pad(key, key_width), Style::new().fg(theme::DARK_GRAY)),
                Span::raw("  "),
                Span::raw(*action),
            ]));
        }
    }
    let mut footer = vec![mode_tag("HELP", theme::YELLOW)];
    footer.extend(hints([("?", "close")]));
    let footer = Line::from(footer);
    let (list, status) = popup(frame, "keys", &lines, &footer);
    frame.render_widget(Paragraph::new(lines), list);
    frame.render_widget(footer, status);
}
