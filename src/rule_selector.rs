//! Bounded selection along a source path's ancestor chain; no free-form input or I/O.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr as _;

use crate::display::sanitize;
use crate::policy::{PolicyError, RuleAction, RuleDraft, RuleScope};

#[derive(Debug, PartialEq, Eq)]
pub enum SelectorEvent {
    Continue,
    Changed,
    Cancelled,
    Submit(RuleDraft),
}

pub struct RuleSelector {
    draft: RuleDraft,
    paths: Vec<String>,
    selected: usize,
    source_scope: RuleScope,
    scroll: u16,
}

impl RuleSelector {
    /// Captures the immutable ancestor chain, longest path first.
    ///
    /// # Errors
    ///
    /// Rejects invalid source paths.
    pub fn new(mut draft: RuleDraft) -> Result<Self, PolicyError> {
        let paths = draft.ancestors()?;
        draft.path.clone_from(&paths[0]);
        Ok(Self {
            source_scope: draft.scope,
            draft,
            paths,
            selected: 0,
            scroll: 0,
        })
    }

    #[must_use]
    pub fn draft(&self) -> &RuleDraft {
        &self.draft
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> SelectorEvent {
        if key.kind == KeyEventKind::Release
            || key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
        {
            return SelectorEvent::Continue;
        }
        match key.code {
            KeyCode::Esc => return SelectorEvent::Cancelled,
            KeyCode::Left | KeyCode::Char('h') => {
                self.selected = (self.selected + 1).min(self.paths.len() - 1);
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.selected = self.selected.saturating_sub(1);
            }
            KeyCode::Char(' ') if self.selected == 0 && key.kind == KeyEventKind::Press => {
                self.source_scope = match self.source_scope {
                    RuleScope::Path => RuleScope::Directory,
                    RuleScope::Directory => RuleScope::Path,
                };
            }
            KeyCode::Char(action @ ('a' | 'd')) if key.kind == KeyEventKind::Press => {
                let mut draft = self.draft.clone();
                draft.action = if action == 'a' {
                    RuleAction::Allow
                } else {
                    RuleAction::Deny
                };
                return SelectorEvent::Submit(draft);
            }
            KeyCode::PageUp | KeyCode::PageDown => {
                self.scroll = if key.code == KeyCode::PageUp {
                    self.scroll.saturating_sub(5)
                } else {
                    self.scroll.saturating_add(5)
                };
                return SelectorEvent::Continue;
            }
            _ => return SelectorEvent::Continue,
        }
        let scope = if self.selected == 0 {
            self.source_scope
        } else {
            RuleScope::Directory
        };
        if self.draft.path == self.paths[self.selected] && self.draft.scope == scope {
            return SelectorEvent::Continue;
        }
        self.draft.path.clone_from(&self.paths[self.selected]);
        self.draft.scope = scope;
        self.scroll = 0;
        SelectorEvent::Changed
    }

    pub fn render(&mut self, frame: &mut Frame<'_>, area: Rect, blocked: Option<&str>) {
        let block = Block::default().borders(Borders::ALL).title(" Rule scope ");
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.is_empty() {
            return;
        }
        let mut hints = vec![
            "Left/h parent | Right/l deeper".to_owned(),
            "a approve + remember | d deny + remember".to_owned(),
            "Esc back | PgUp/PgDn scroll".to_owned(),
        ];
        if self.selected == 0 {
            hints.push("Space exact/tree".to_owned());
        }
        let hints = wrapped_lines(hints, usize::from(inner.width));
        let hint_height = u16::try_from(hints.len()).unwrap_or(u16::MAX);
        let [body, footer] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(hint_height)]).areas(inner);
        let scope = match self.draft.scope {
            RuleScope::Path => "Exact path",
            RuleScope::Directory => "Tree (path + descendants)",
        };
        let mut lines = wrapped_lines(
            [format!("{} | {scope}", self.draft.access)],
            usize::from(body.width.max(1)),
        );
        lines.extend(
            path_lines(&self.draft.path, usize::from(body.width.max(1)))
                .into_iter()
                .map(|line| line.style(Style::default().add_modifier(Modifier::BOLD))),
        );
        let boundary = if self.selected == 0 {
            "Original request path"
        } else if self.selected + 1 == self.paths.len() {
            "Root path: no parent"
        } else {
            "Ancestor of request path"
        };
        lines.extend(wrapped_lines(
            [boundary.to_owned()],
            usize::from(body.width.max(1)),
        ));
        if self.draft.path == "/" && self.draft.scope == RuleScope::Directory {
            lines.extend(
                wrapped_lines(
                    ["Warning: all absolute paths for this access".to_owned()],
                    usize::from(body.width.max(1)),
                )
                .into_iter()
                .map(|line| line.style(Style::default().fg(Color::Yellow))),
            );
        }
        if let Some(error) = blocked {
            let mut feedback = wrapped_lines(
                [format!("Blocked: {}", sanitize(error))],
                usize::from(body.width.max(1)),
            );
            for line in &mut feedback {
                line.style = Style::default().fg(Color::Red);
            }
            feedback.extend(lines);
            lines = feedback;
        }
        let max_scroll = lines.len().saturating_sub(usize::from(body.height));
        self.scroll = self
            .scroll
            .min(u16::try_from(max_scroll).unwrap_or(u16::MAX));
        frame.render_widget(Paragraph::new(lines).scroll((self.scroll, 0)), body);
        frame.render_widget(Paragraph::new(hints).wrap(Wrap { trim: false }), footer);
    }
}

fn wrapped_lines(text: impl IntoIterator<Item = String>, width: usize) -> Vec<Line<'static>> {
    text.into_iter()
        .flat_map(|text| {
            textwrap::wrap(&text, width.max(1))
                .into_iter()
                .map(|line| Line::from(line.into_owned()))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Lossless visible escaping; selection always uses the original path data.
#[must_use]
pub fn display_path(path: &str) -> String {
    path.chars().flat_map(char::escape_debug).collect()
}

fn path_lines(path: &str, width: usize) -> Vec<Line<'static>> {
    path_lines_display(&display_path(path), width)
}

fn path_lines_display(text: &str, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut spans = Vec::new();
    let mut column = 0;
    for character in text.chars() {
        let text = character.to_string();
        let cells = text.width();
        if column + cells > width && column > 0 {
            lines.push(Line::from(std::mem::take(&mut spans)));
            column = 0;
        }
        spans.push(Span::raw(text));
        column += cells;
    }
    lines.push(Line::from(spans));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::AccessMode;
    use ratatui::{Terminal, backend::TestBackend};

    fn selector(path: &str) -> RuleSelector {
        RuleSelector::new(RuleDraft {
            action: RuleAction::Allow,
            path: path.to_owned(),
            scope: RuleScope::Path,
            access: AccessMode::Read,
        })
        .unwrap()
    }

    fn press(selector: &mut RuleSelector, code: KeyCode) -> SelectorEvent {
        selector.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn navigation_is_bounded_and_reversible_without_changing_the_source_chain() {
        let mut selector = selector("/work/src/main.rs");
        for code in [KeyCode::Left, KeyCode::Char('h')] {
            assert_eq!(press(&mut selector, code), SelectorEvent::Changed);
        }
        assert_eq!(selector.draft.path, "/work");
        assert_eq!(selector.draft.scope, RuleScope::Directory);
        assert_eq!(
            press(&mut selector, KeyCode::Char(' ')),
            SelectorEvent::Continue
        );
        for _ in 0..10 {
            press(&mut selector, KeyCode::Left);
        }
        assert_eq!(selector.draft.path, "/");
        for code in [KeyCode::Right, KeyCode::Char('l'), KeyCode::Right] {
            assert_eq!(press(&mut selector, code), SelectorEvent::Changed);
        }
        assert_eq!(selector.draft.path, "/work/src/main.rs");
        assert_eq!(selector.draft.scope, RuleScope::Path);
        assert_eq!(
            press(&mut selector, KeyCode::Right),
            SelectorEvent::Continue
        );
    }

    #[test]
    fn original_scope_is_restored_and_root_source_is_bounded() {
        let mut selector = selector("/");
        assert_eq!(press(&mut selector, KeyCode::Left), SelectorEvent::Continue);
        assert_eq!(
            press(&mut selector, KeyCode::Right),
            SelectorEvent::Continue
        );
        assert_eq!(
            press(&mut selector, KeyCode::Char(' ')),
            SelectorEvent::Changed
        );
        assert_eq!(selector.draft.scope, RuleScope::Directory);
        let mut selector = self::selector("/work/src");
        press(&mut selector, KeyCode::Char(' '));
        press(&mut selector, KeyCode::Left);
        press(&mut selector, KeyCode::Right);
        assert_eq!(selector.draft.scope, RuleScope::Directory);
    }

    #[test]
    fn only_explicit_action_keys_submit_and_text_never_edits_paths() {
        let mut selector = selector("/work/data");
        for code in [
            KeyCode::Enter,
            KeyCode::Tab,
            KeyCode::Backspace,
            KeyCode::Delete,
            KeyCode::Char('x'),
        ] {
            assert_eq!(press(&mut selector, code), SelectorEvent::Continue);
        }
        for action in ['a', 'd'] {
            for modifiers in [
                KeyModifiers::CONTROL,
                KeyModifiers::ALT,
                KeyModifiers::SUPER,
            ] {
                assert_eq!(
                    selector.handle_key(KeyEvent::new(KeyCode::Char(action), modifiers)),
                    SelectorEvent::Continue
                );
            }
            for kind in [KeyEventKind::Release, KeyEventKind::Repeat] {
                assert_eq!(
                    selector.handle_key(KeyEvent::new_with_kind(
                        KeyCode::Char(action),
                        KeyModifiers::NONE,
                        kind
                    )),
                    SelectorEvent::Continue
                );
            }
            let SelectorEvent::Submit(draft) = press(&mut selector, KeyCode::Char(action)) else {
                panic!("missing submit");
            };
            assert_eq!(draft.path, "/work/data");
            assert_eq!(
                draft.action,
                if action == 'a' {
                    RuleAction::Allow
                } else {
                    RuleAction::Deny
                }
            );
        }
        assert_eq!(press(&mut selector, KeyCode::Esc), SelectorEvent::Cancelled);
    }

    #[test]
    fn literal_paths_round_trip_and_wrap_losslessly() {
        let path = "/work/中文/e\u{301}/\u{1b}[31m\\literal\n*";
        let mut selector = selector(path);
        press(&mut selector, KeyCode::Left);
        press(&mut selector, KeyCode::Right);
        assert_eq!(selector.draft.path, path);
        let lines = path_lines(path, 12);
        assert!(lines.iter().all(|line| line.width() <= 12));
        assert_eq!(
            lines.iter().map(ToString::to_string).collect::<String>(),
            display_path(path)
        );
        assert!(!display_path(path).contains('\u{1b}'));
    }

    #[test]
    fn normalized_choices_always_cover_the_source() {
        use crate::protocol::parse_default_webhook_body;
        let request = serde_json::json!({"backend":"test", "request": {
            "capability_type":"capability", "request_id":"source", "session_id":"test",
            "child_pid":1, "path":"/work//./src/main.rs/", "access":"Read"
        }});
        let source = parse_default_webhook_body(&serde_json::to_vec(&request).unwrap()).unwrap();
        let draft =
            RuleDraft::from_request(&source.request, RuleAction::Allow, RuleScope::Path).unwrap();
        let mut selector = RuleSelector::new(draft).unwrap();
        assert_eq!(selector.draft.path, "/work/src/main.rs");
        for code in [
            KeyCode::Left,
            KeyCode::Left,
            KeyCode::Left,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Right,
            KeyCode::Right,
            KeyCode::Right,
        ] {
            press(&mut selector, code);
            selector.draft.validate_source(&source.request).unwrap();
        }
        assert_eq!(selector.draft.path, "/work/src/main.rs");
        for path in ["relative", "/work/../other", "/work/\0"] {
            let mut draft = selector.draft.clone();
            draft.path = path.to_owned();
            assert!(RuleSelector::new(draft).is_err());
        }
    }

    #[test]
    fn long_paths_scroll_without_hiding_controls_and_errors_are_readable() {
        let path = format!("/{}", "long directory/".repeat(25));
        let mut selector = selector(&path);
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        let lines = path_lines(&path, 38);
        assert!(lines.iter().all(|line| line.width() <= 38));
        assert_eq!(
            lines.iter().map(ToString::to_string).collect::<String>(),
            display_path(&path)
        );
        for error in [None, Some("Source request is no longer pending")] {
            for code in [KeyCode::PageDown, KeyCode::PageDown, KeyCode::PageUp] {
                press(&mut selector, code);
                terminal
                    .draw(|frame| selector.render(frame, frame.area(), error))
                    .unwrap();
                let text = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(ratatui::buffer::Cell::symbol)
                    .collect::<String>();
                for expected in ["Left/h", "Right/l", "a approve", "d deny", "Esc back"] {
                    assert!(text.contains(expected));
                }
            }
        }
        for _ in 0..20 {
            press(&mut selector, KeyCode::PageUp);
        }
        terminal
            .draw(|frame| {
                selector.render(
                    frame,
                    frame.area(),
                    Some("Source request is no longer pending"),
                );
            })
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        assert!(text.contains("Blocked: Source request"));
        assert!(text.contains("pending"));
    }

    #[test]
    fn frames_keep_path_scope_and_actions_visible() {
        for (width, height) in [(120, 24), (80, 24), (60, 24), (40, 12)] {
            let mut selector = selector("/work/src/main.rs");
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            for _ in 0..4 {
                terminal
                    .draw(|frame| selector.render(frame, frame.area(), None))
                    .unwrap();
                let text = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(ratatui::buffer::Cell::symbol)
                    .collect::<String>();
                for expected in [
                    "Rule scope",
                    "read",
                    &selector.draft.path,
                    "Left/h",
                    "Right/l",
                    "a approve",
                    "d deny",
                    "Esc back",
                ] {
                    assert!(
                        text.contains(expected),
                        "missing {expected} at {width}x{height}"
                    );
                }
                press(&mut selector, KeyCode::Left);
            }
        }
    }
}
