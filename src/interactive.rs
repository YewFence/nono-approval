use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use jiff::Timestamp;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use thiserror::Error;

use crate::broker::{
    ApprovalDetail, ApprovalId, ApprovalSummary, DEFAULT_DENIAL_REASON, validate_denial_reason,
};
use crate::control::{
    ApprovalView, ControlClient, ControlClientError, DebugCaptureStatus, DecisionRequest,
};
use crate::display::{sanitize, truncate_summary};

const CONNECTED_POLL: Duration = Duration::from_millis(500);
const DISCONNECTED_POLL: Duration = Duration::from_secs(1);

#[derive(Debug, Error)]
pub enum InteractiveError {
    #[error(transparent)]
    Io(#[from] io::Error),
}

struct ReasonInput {
    approval_id: ApprovalId,
    value: String,
    error: Option<String>,
}

struct App {
    client: ControlClient,
    connected: bool,
    approvals: Vec<ApprovalSummary>,
    selected: Option<usize>,
    detail: Option<ApprovalDetail>,
    detail_scroll: u16,
    show_detail_panel: bool,
    reason: Option<ReasonInput>,
    status: String,
    debug_capture: DebugCaptureStatus,
    next_poll: Instant,
}

impl App {
    fn new(socket_path: &Path) -> Self {
        Self {
            client: ControlClient::new(socket_path),
            connected: false,
            approvals: Vec::new(),
            selected: None,
            detail: None,
            detail_scroll: 0,
            show_detail_panel: false,
            reason: None,
            status: "Disconnected — waiting for daemon…".to_owned(),
            debug_capture: DebugCaptureStatus::Disabled,
            next_poll: Instant::now(),
        }
    }

    fn selected_id(&self) -> Option<ApprovalId> {
        self.selected
            .and_then(|index| self.approvals.get(index))
            .map(|approval| approval.approval_id.clone())
    }

    fn disconnect(&mut self) {
        self.connected = false;
        self.approvals.clear();
        self.selected = None;
        self.detail = None;
        self.detail_scroll = 0;
        self.reason = None;
        "Disconnected — waiting for daemon…".clone_into(&mut self.status);
        self.next_poll = Instant::now() + DISCONNECTED_POLL;
    }

    async fn refresh(&mut self) {
        let old_id = self.selected_id();
        let old_index = self.selected.unwrap_or_default();
        let Ok(list) = self.client.list().await else {
            self.disconnect();
            return;
        };
        self.connected = true;
        self.approvals = list.approvals;
        self.selected = if self.approvals.is_empty() {
            None
        } else if let Some(old_id) = old_id {
            self.approvals
                .iter()
                .position(|approval| approval.approval_id == old_id)
                .or_else(|| Some(old_index.min(self.approvals.len() - 1)))
        } else {
            Some(0)
        };
        let Ok(status) = self.client.status().await else {
            self.disconnect();
            return;
        };
        self.debug_capture = status.debug_capture;
        if self.refresh_detail().await.is_err() {
            self.disconnect();
            return;
        }
        self.status = if self.approvals.is_empty() {
            "Waiting for approval requests…".to_owned()
        } else {
            "a approve · d deny · D deny with reason · q quit".to_owned()
        };
        self.next_poll = Instant::now() + CONNECTED_POLL;
    }

    async fn refresh_detail(&mut self) -> Result<(), ControlClientError> {
        let Some(approval_id) = self.selected_id() else {
            self.detail = None;
            return Ok(());
        };
        self.detail = match self.client.show(&approval_id, false).await {
            Ok(ApprovalView::Pending(detail)) => Some(*detail),
            Ok(ApprovalView::Completed(_))
            | Err(ControlClientError::Response {
                status: hyper::StatusCode::NOT_FOUND,
                ..
            }) => None,
            Err(error) => return Err(error),
        };
        Ok(())
    }

    async fn decide(&mut self, approval_id: ApprovalId, decision: DecisionRequest) {
        self.status = match self.client.decide(&approval_id, &decision).await {
            Ok(response) => format!("{}: {:?}", response.approval_id, response.state),
            Err(error) => format!("Decision failed: {error}"),
        };
        self.next_poll = Instant::now();
    }

    fn move_selection(&mut self, delta: isize) {
        let Some(selected) = self.selected else {
            return;
        };
        let maximum = self.approvals.len().saturating_sub(1);
        let next = selected.saturating_add_signed(delta).min(maximum);
        if next != selected {
            self.selected = Some(next);
            self.detail_scroll = 0;
            self.next_poll = Instant::now();
        }
    }
}

/// Runs the full-screen approval client until the user quits or a signal arrives.
///
/// # Errors
///
/// Returns an error when terminal initialization, drawing, or event input fails.
pub async fn run(socket_path: &Path) -> Result<(), InteractiveError> {
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|panic| {
        ratatui::restore();
        eprintln!(
            "nono-approval TUI panicked: {}",
            sanitize(&panic.to_string())
        );
    }));
    let mut terminal = ratatui::try_init()?;
    let result = run_loop(&mut terminal, socket_path).await;
    ratatui::restore();
    let _ = std::panic::take_hook();
    std::panic::set_hook(previous_hook);
    result
}

async fn run_loop(
    terminal: &mut ratatui::DefaultTerminal,
    socket_path: &Path,
) -> Result<(), InteractiveError> {
    let mut app = App::new(socket_path);
    loop {
        if Instant::now() >= app.next_poll {
            app.refresh().await;
        }
        terminal.draw(|frame| render(frame, &app))?;
        if event::poll(Duration::from_millis(50))?
            && let Event::Key(key) = event::read()?
            && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
            && handle_key(&mut app, key).await
        {
            return Ok(());
        }
    }
}

async fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if let Some(reason) = &mut app.reason {
        match key.code {
            KeyCode::Esc => app.reason = None,
            KeyCode::Backspace => {
                reason.value.pop();
                reason.error = None;
            }
            KeyCode::Enter => {
                if let Err(error) = validate_denial_reason(&reason.value) {
                    reason.error = Some(error.to_string());
                } else {
                    let approval_id = reason.approval_id.clone();
                    let value = reason.value.clone();
                    app.reason = None;
                    app.decide(approval_id, DecisionRequest::Denied { reason: value })
                        .await;
                }
            }
            KeyCode::Char(character)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && reason.value.len() + character.len_utf8() <= 4 * 1024 =>
            {
                reason.value.push(character);
                reason.error = None;
            }
            KeyCode::Char(_) => {
                reason.error = Some("Reason is limited to 4 KiB".to_owned());
            }
            _ => {}
        }
        return false;
    }

    match (key.code, key.modifiers) {
        (KeyCode::Char('q'), _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,
        (KeyCode::Down | KeyCode::Char('j'), _) => app.move_selection(1),
        (KeyCode::Up | KeyCode::Char('k'), _) => app.move_selection(-1),
        (KeyCode::Tab, _) => app.show_detail_panel = !app.show_detail_panel,
        (KeyCode::PageDown, _) => app.detail_scroll = app.detail_scroll.saturating_add(20),
        (KeyCode::PageUp, _) => app.detail_scroll = app.detail_scroll.saturating_sub(20),
        (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
            app.detail_scroll = app.detail_scroll.saturating_add(10);
        }
        (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
            app.detail_scroll = app.detail_scroll.saturating_sub(10);
        }
        (KeyCode::Char('g'), _) => app.detail_scroll = 0,
        (KeyCode::Char('G'), _) => app.detail_scroll = u16::MAX,
        (KeyCode::Char('a'), _) => {
            if let Some(approval_id) = app.selected_id() {
                app.decide(approval_id, DecisionRequest::Granted).await;
            }
        }
        (KeyCode::Char('d'), _) => {
            if let Some(approval_id) = app.selected_id() {
                app.decide(
                    approval_id,
                    DecisionRequest::Denied {
                        reason: DEFAULT_DENIAL_REASON.to_owned(),
                    },
                )
                .await;
            }
        }
        (KeyCode::Char('D'), _) => {
            if let Some(approval_id) = app.selected_id() {
                app.reason = Some(ReasonInput {
                    approval_id,
                    value: String::new(),
                    error: None,
                });
            }
        }
        _ => {}
    }
    false
}

fn render(frame: &mut Frame<'_>, app: &App) {
    let [main, footer] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(2)]).areas(frame.area());
    if main.width >= 90 {
        let [queue, detail] =
            Layout::horizontal([Constraint::Percentage(38), Constraint::Percentage(62)])
                .areas(main);
        render_queue(frame, app, queue);
        render_detail(frame, app, detail);
    } else if app.show_detail_panel {
        render_detail(frame, app, main);
    } else {
        render_queue(frame, app, main);
    }
    render_footer(frame, app, footer);
}

fn render_queue(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let items = app
        .approvals
        .iter()
        .map(|approval| {
            let prefix = format!("{} · ", approval.capability_type);
            let available = usize::from(area.width.saturating_sub(2));
            let summary_width =
                available.saturating_sub(unicode_width::UnicodeWidthStr::width(prefix.as_str()));
            ListItem::new(vec![
                Line::from(Span::styled(
                    approval.approval_id.to_string(),
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Line::from(format!(
                    "{prefix}{}",
                    truncate_summary(&approval.summary, summary_width)
                )),
            ])
        })
        .collect::<Vec<_>>();
    let title = if app.connected {
        " Pending approvals "
    } else {
        " Disconnected — waiting for daemon… "
    };
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan));
    let mut state = ListState::default().with_selected(app.selected);
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_detail(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let text = app.detail.as_ref().map_or_else(
        || Text::from("No pending approval selected."),
        |detail| {
            let mut lines = vec![Line::from(Span::styled(
                detail.approval_id.to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ))];
            for field in &detail.content.fields {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("{}: ", field.label),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(field.value.clone()),
                ]));
            }
            lines.push(Line::from(format!("Deadline: {}", detail.deadline)));
            lines.push(Line::from(format!(
                "Lease remaining: {}",
                lease_remaining(&detail.deadline)
            )));
            Text::from(lines)
        },
    );
    let paragraph = Paragraph::new(text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Decision detail "),
        )
        .wrap(Wrap { trim: false })
        .scroll((app.detail_scroll, 0));
    frame.render_widget(paragraph, area);
}

fn render_footer(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let message = app.reason.as_ref().map_or_else(
        || {
            let capture = match &app.debug_capture {
                DebugCaptureStatus::Failed { .. } => " · debug capture: failed",
                DebugCaptureStatus::Enabled { .. } => " · debug capture: enabled",
                DebugCaptureStatus::Disabled => "",
            };
            format!("{}{}", sanitize(&app.status), capture)
        },
        |reason| {
            reason.error.as_ref().map_or_else(
                || format!("Deny reason: {}", sanitize(&reason.value)),
                |error| format!("Deny reason: {} · {error}", sanitize(&reason.value)),
            )
        },
    );
    frame.render_widget(Paragraph::new(message).wrap(Wrap { trim: false }), area);
}

fn lease_remaining(deadline: &str) -> String {
    let Ok(deadline) = deadline.parse::<Timestamp>() else {
        return "unknown".to_owned();
    };
    let seconds = deadline.duration_since(Timestamp::now()).as_secs().max(0);
    format!("{}m {:02}s", seconds / 60, seconds % 60)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::{App, ReasonInput, handle_key, render};
    use crate::broker::{ApprovalId, ApprovalSummary};

    fn approval() -> ApprovalSummary {
        ApprovalSummary {
            approval_id: "appr_0123456789abcdef".parse().unwrap(),
            capability_type: "command".to_owned(),
            summary: "a very long request summary that must visibly truncate".to_owned(),
            received_at: "2026-07-29T00:00:00Z".to_owned(),
            deadline: "2026-07-29T00:04:30Z".to_owned(),
        }
    }

    #[test]
    fn disconnect_clears_all_request_state() {
        let mut app = App::new(Path::new("/tmp/unreachable-control.sock"));
        app.connected = true;
        app.approvals.push(approval());
        app.selected = Some(0);
        app.detail_scroll = 42;
        app.show_detail_panel = true;
        app.reason = Some(ReasonInput {
            approval_id: "appr_0123456789abcdef".parse().unwrap(),
            value: "draft".to_owned(),
            error: None,
        });
        app.disconnect();
        assert!(!app.connected);
        assert!(app.approvals.is_empty());
        assert!(app.detail.is_none());
        assert_eq!(app.detail_scroll, 0);
        assert!(app.selected.is_none());
        assert!(app.reason.is_none());
    }

    #[test]
    fn renders_stable_wide_and_narrow_layouts() {
        for (width, expected) in [(100, "Decision detail"), (36, "Pending approvals")] {
            let backend = TestBackend::new(width, 12);
            let mut terminal = Terminal::new(backend).unwrap();
            let mut app = App::new(Path::new("/tmp/unreachable-control.sock"));
            app.connected = true;
            app.approvals.push(approval());
            app.selected = Some(0);
            terminal.draw(|frame| render(frame, &app)).unwrap();
            let rendered = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>();
            assert!(rendered.contains(expected));
            if width == 36 {
                assert!(rendered.contains('…'));
            }
        }
    }

    #[tokio::test]
    async fn enter_never_approves_and_nul_only_reason_stays_in_editor() {
        let mut app = App::new(Path::new("/tmp/unreachable-control.sock"));
        app.approvals.push(approval());
        app.selected = Some(0);
        assert!(!handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).await);

        app.reason = Some(ReasonInput {
            approval_id: "appr_0123456789abcdef".parse::<ApprovalId>().unwrap(),
            value: "\0\0".to_owned(),
            error: None,
        });
        assert!(!handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).await);
        assert!(app.reason.as_ref().unwrap().error.is_some());
    }
}
