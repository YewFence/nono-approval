use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use jiff::Timestamp;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use thiserror::Error;

use crate::broker::{
    ApprovalDetail, ApprovalId, ApprovalSummary, DEFAULT_DENIAL_REASON, validate_denial_reason,
};
use crate::control::{
    ApprovalView, ControlClient, ControlClientError, DebugCaptureStatus, DecisionRequest,
    SessionRuleRequest,
};
use crate::display::{sanitize, truncate_summary};
use crate::policy::{PolicyFile, RuleAction, RuleDraft, RuleScope};
use crate::protocol::KnownApprovalRequest;
use crate::rule_selector::{RuleSelector, SelectorEvent, display_path};
use crate::runtime_path::ProjectPaths;

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

struct SaveInput {
    value: String,
    error: Option<String>,
}

struct ApprovalRuleEditing {
    approval_id: ApprovalId,
    request: KnownApprovalRequest,
    deadline: String,
    available: bool,
    source_scroll: u16,
    editor: RuleSelector,
    error: Option<String>,
    too_small: bool,
}

impl ApprovalRuleEditing {
    fn blocked(&self) -> Option<String> {
        if !self.available {
            return Some("Source request is no longer pending".to_owned());
        }
        self.editor
            .draft()
            .validate_source(&self.request)
            .err()
            .map(|error| error.to_string())
            .or_else(|| self.error.clone())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClearConfirmation {
    Closed,
    Hidden,
    Visible,
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
    save: Option<SaveInput>,
    editing: Option<ApprovalRuleEditing>,
    clear_confirmation: ClearConfirmation,
    status: String,
    debug_capture: DebugCaptureStatus,
    session_rule_count: usize,
    status_until: Instant,
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
            save: None,
            editing: None,
            clear_confirmation: ClearConfirmation::Closed,
            status: "Disconnected — waiting for daemon…".to_owned(),
            debug_capture: DebugCaptureStatus::Disabled,
            session_rule_count: 0,
            status_until: Instant::now(),
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
        self.editing = None;
        self.clear_confirmation = ClearConfirmation::Closed;
        self.session_rule_count = 0;
        if Instant::now() >= self.status_until {
            "Disconnected — waiting for daemon…".clone_into(&mut self.status);
        }
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
        self.selected = if let Some(editing) = &mut self.editing {
            let selected = self
                .approvals
                .iter()
                .position(|item| item.approval_id == editing.approval_id);
            editing.available &= selected.is_some();
            selected
        } else if self.approvals.is_empty() {
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
        self.session_rule_count = status.session_rule_count;
        if self.editing.is_none() && self.refresh_detail().await.is_err() {
            self.disconnect();
            return;
        }
        if Instant::now() >= self.status_until {
            self.status = if self.approvals.is_empty() {
                "Waiting for approval requests…".to_owned()
            } else {
                "a approve · d deny · D reason · r rule · q quit".to_owned()
            };
        }
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
        self.status_until = Instant::now() + Duration::from_secs(4);
    }

    async fn clear_session_rules(&mut self) {
        self.clear_confirmation = ClearConfirmation::Closed;
        self.status = match self.client.clear_session_rules().await {
            Ok(response) => {
                self.session_rule_count = 0;
                format!("Cleared {} session rule(s).", response.cleared)
            }
            Err(error) => format!("Clear session rules failed: {error}"),
        };
        self.next_poll = Instant::now();
        self.status_until = Instant::now() + Duration::from_secs(4);
    }

    async fn save_session_rules(&mut self, value: &str) {
        self.status = match Self::write_session_rules(&self.client, value).await {
            Ok(path) => format!("Saved session rules to {path}"),
            Err(error) => format!("Save rules failed: {error}"),
        };
        self.status_until = Instant::now() + Duration::from_secs(4);
    }

    async fn write_session_rules(client: &ControlClient, value: &str) -> Result<String, String> {
        let rules = client
            .session_rules()
            .await
            .map_err(|error| error.to_string())?
            .rules;
        if rules.is_empty() {
            return Err("no session rules".to_owned());
        }
        let paths = ProjectPaths::resolve().map_err(|error| error.to_string())?;
        let path = resolve_save_path(value, &paths);
        let contents =
            toml::to_string_pretty(&PolicyFile { rules }).map_err(|error| error.to_string())?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| error.to_string())?;
        if let Err(error) = file.write_all(contents.as_bytes()) {
            // A create_new file already exists on disk; do not leave a partial
            // policy behind or the next save to the same name fails forever.
            let _ = std::fs::remove_file(&path);
            return Err(error.to_string());
        }
        Ok(sanitize(&path.display().to_string()))
    }

    async fn open_rule_editor(&mut self, action: RuleAction, scope: RuleScope) {
        let Some(approval_id) = self.selected_id() else {
            return;
        };
        let result = self.client.show(&approval_id, true).await;
        self.status = match result {
            Ok(ApprovalView::Pending(detail)) => {
                if let Some(debug) = detail.debug {
                    match RuleDraft::from_request(&debug.wire_request, action, scope)
                        .and_then(RuleSelector::new)
                    {
                        Ok(editor) => {
                            self.editing = Some(ApprovalRuleEditing {
                                approval_id,
                                request: debug.wire_request,
                                deadline: detail.deadline,
                                available: true,
                                source_scroll: 0,
                                editor,
                                error: None,
                                too_small: false,
                            });
                            self.next_poll = Instant::now();
                            return;
                        }
                        Err(error) => format!("Rule draft failed: {error}"),
                    }
                } else {
                    "Rule draft failed: source path is unavailable".to_owned()
                }
            }
            Ok(ApprovalView::Completed(_)) => "Source request is no longer pending".to_owned(),
            Err(error) => format!("Rule draft failed: {error}"),
        };
        self.status_until = Instant::now() + Duration::from_secs(4);
        self.next_poll = Instant::now();
    }

    async fn edit_rule(&mut self, key: KeyEvent) {
        let Some(editing) = &mut self.editing else {
            return;
        };
        if editing.too_small && key.code != KeyCode::Esc {
            return;
        }
        if key.modifiers == KeyModifiers::CONTROL && matches!(key.code, KeyCode::Up | KeyCode::Down)
        {
            editing.source_scroll = if key.code == KeyCode::Up {
                editing.source_scroll.saturating_sub(1)
            } else {
                editing.source_scroll.saturating_add(1)
            };
            return;
        }
        match editing.editor.handle_key(key) {
            SelectorEvent::Continue => {}
            SelectorEvent::Changed => editing.error = None,
            SelectorEvent::Cancelled => {
                self.editing = None;
                self.next_poll = Instant::now();
            }
            SelectorEvent::Submit(draft) => {
                editing.error = None;
                if let Some(error) = editing.blocked() {
                    editing.error = Some(error);
                    return;
                }
                self.submit_rule(draft).await;
            }
        }
    }

    async fn submit_rule(&mut self, draft: RuleDraft) {
        let Some(editing) = &mut self.editing else {
            return;
        };
        let request = SessionRuleRequest {
            action: draft.action,
            scope: draft.scope,
            reason: None,
            path: Some(draft.path),
        };
        match self.client.remember(&editing.approval_id, &request).await {
            Ok(_) => {
                self.status = format!(
                    "{}: {:?} {:?} rule remembered",
                    editing.approval_id, draft.action, draft.scope
                );
                self.editing = None;
            }
            Err(error) => editing.error = Some(format!("Session rule failed: {error}")),
        }
        self.status_until = Instant::now() + Duration::from_secs(4);
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
        terminal.draw(|frame| render(frame, &mut app))?;
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
    if app.save.is_some() {
        return handle_save_key(app, key).await;
    }
    if app.clear_confirmation != ClearConfirmation::Closed {
        return handle_clear_rules_key(app, key).await;
    }
    if app.editing.is_some() {
        if key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::CONTROL {
            return true;
        }
        app.edit_rule(key).await;
        return false;
    }
    if app.reason.is_some() {
        handle_reason_key(app, key).await;
        return false;
    }

    if key.kind != KeyEventKind::Press
        && matches!(key.code, KeyCode::Char('a' | 'd' | 'D'))
        && !key.modifiers.contains(KeyModifiers::CONTROL)
    {
        return false;
    }
    match (key.code, key.modifiers) {
        (KeyCode::Char('q'), _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,
        (KeyCode::Char('C'), KeyModifiers::NONE | KeyModifiers::SHIFT)
            if key.kind == KeyEventKind::Press && app.connected =>
        {
            app.clear_confirmation = ClearConfirmation::Hidden;
        }
        (KeyCode::Char('S'), KeyModifiers::NONE | KeyModifiers::SHIFT)
            if key.kind == KeyEventKind::Press && app.connected =>
        {
            app.save = Some(SaveInput {
                value: String::new(),
                error: None,
            });
        }
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
        (KeyCode::Char('a'), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
            if let Some(approval_id) = app.selected_id() {
                app.decide(approval_id, DecisionRequest::Granted).await;
            }
        }
        (
            KeyCode::Char(shortcut @ ('r' | 'p' | 'P' | 'A')),
            KeyModifiers::NONE | KeyModifiers::SHIFT,
        ) if key.kind == KeyEventKind::Press => {
            let (action, scope) = match shortcut {
                'p' => (RuleAction::Deny, RuleScope::Path),
                'P' => (RuleAction::Deny, RuleScope::Directory),
                'A' => (RuleAction::Allow, RuleScope::Directory),
                _ => (RuleAction::Allow, RuleScope::Path),
            };
            app.open_rule_editor(action, scope).await;
        }
        (KeyCode::Char('d'), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
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
        (KeyCode::Char('D'), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
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

async fn handle_save_key(app: &mut App, key: KeyEvent) -> bool {
    let Some(save) = &mut app.save else {
        return false;
    };
    match key.code {
        KeyCode::Esc => app.save = None,
        KeyCode::Backspace => {
            save.value.pop();
            save.error = None;
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => save.value.push(c),
        KeyCode::Enter => {
            if save.value.is_empty() {
                save.error = Some("Path must not be empty".to_owned());
            } else {
                let value = save.value.clone();
                app.save = None;
                app.save_session_rules(&value).await;
            }
        }
        _ => {}
    }
    false
}

fn resolve_save_path(value: &str, paths: &ProjectPaths) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() || value.starts_with("./") || value.starts_with("../") {
        return path;
    }
    if path.components().count() == 1 {
        return paths.config_file.parent().unwrap().join(format!(
            "{}.toml",
            value.strip_suffix(".toml").unwrap_or(value)
        ));
    }
    path
}

async fn handle_clear_rules_key(app: &mut App, key: KeyEvent) -> bool {
    if key.kind != KeyEventKind::Press {
        return false;
    }
    match (key.code, key.modifiers) {
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,
        (KeyCode::Char('y'), KeyModifiers::NONE)
            if app.connected && app.clear_confirmation == ClearConfirmation::Visible =>
        {
            app.clear_session_rules().await;
        }
        (KeyCode::Esc | KeyCode::Char('n'), KeyModifiers::NONE) => {
            app.clear_confirmation = ClearConfirmation::Closed;
        }
        _ => {}
    }
    false
}

async fn handle_reason_key(app: &mut App, key: KeyEvent) {
    let Some(reason) = &mut app.reason else {
        return;
    };
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
}

fn render(frame: &mut Frame<'_>, app: &mut App) {
    if let Some(editing) = &mut app.editing {
        render_rule_editing(frame, editing, app.session_rule_count, &app.debug_capture);
        return;
    }
    let message = footer_message(app);
    let lines = textwrap::wrap(&message, usize::from(frame.area().width.max(1))).len();
    let footer_height = u16::try_from(lines.clamp(2, 6))
        .unwrap_or(6)
        .min(frame.area().height.saturating_sub(3));
    let [main, footer] = Layout::vertical([Constraint::Min(3), Constraint::Length(footer_height)])
        .areas(frame.area());
    if app.clear_confirmation != ClearConfirmation::Closed {
        app.clear_confirmation = if lines <= usize::from(footer.height) {
            ClearConfirmation::Visible
        } else {
            ClearConfirmation::Hidden
        };
    }
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
    if app.save.is_some() {
        render_save_dialog(frame, app);
    }
}

fn render_rule_editing(
    frame: &mut Frame<'_>,
    editing: &mut ApprovalRuleEditing,
    rule_count: usize,
    capture: &DebugCaptureStatus,
) {
    editing.too_small = frame.area().width < 40 || frame.area().height < 22;
    if editing.too_small {
        frame.render_widget(
            Paragraph::new("Terminal too small: need 40x22\nEsc back | Ctrl-C quit")
                .wrap(Wrap { trim: false }),
            frame.area(),
        );
        return;
    }
    let footer_text = format!(
        "Lifetime: daemon run | session rules: {rule_count} | Lease: {}{} | Ctrl-Up/Down source | Ctrl-C quit",
        lease_remaining(&editing.deadline),
        capture_indicator(capture)
    );
    let footer_height = u16::try_from(
        textwrap::wrap(&footer_text, usize::from(frame.area().width.max(1)))
            .len()
            .clamp(2, 4),
    )
    .unwrap_or(4);
    let [main, footer] = Layout::vertical([Constraint::Min(0), Constraint::Length(footer_height)])
        .areas(frame.area());
    let [source, form] = if main.width >= 90 {
        Layout::horizontal([Constraint::Percentage(38), Constraint::Percentage(62)]).areas(main)
    } else {
        Layout::vertical([
            Constraint::Length((main.height / 4).clamp(3, 6)),
            Constraint::Min(0),
        ])
        .areas(main)
    };
    let mut lines = vec![Line::from(editing.approval_id.to_string())];
    if let KnownApprovalRequest::Capability {
        path,
        access,
        reason,
        ..
    } = &editing.request
    {
        lines.push(Line::from(format!("Access: {access}")));
        lines.push(Line::from(display_path(path)));
        if let Some(reason) = reason {
            lines.push(Line::from(format!("Reason: {}", sanitize(reason))));
        }
    }
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((editing.source_scroll, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Source request "),
            ),
        source,
    );
    let blocked = editing.blocked();
    editing.editor.render(frame, form, blocked.as_deref());
    frame.render_widget(
        Paragraph::new(footer_text).wrap(Wrap { trim: false }),
        footer,
    );
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

fn capture_indicator(capture: &DebugCaptureStatus) -> &'static str {
    match capture {
        DebugCaptureStatus::Failed { .. } => " · debug capture: failed",
        DebugCaptureStatus::Enabled { .. } => " · debug capture: enabled",
        DebugCaptureStatus::Disabled => "",
    }
}

fn footer_message(app: &App) -> String {
    if let Some(save) = &app.save {
        return format!(
            "Save rules to file: {}{} | Enter save · Esc cancel",
            sanitize(&save.value),
            save.error
                .as_ref()
                .map_or(String::new(), |e| format!(" · {e}"))
        );
    }
    if app.clear_confirmation != ClearConfirmation::Closed {
        return format!(
            "Clear ALL {} allow/deny rules in this daemon (all clients)? Pending requests unchanged. y confirm | n/Esc cancel | Ctrl-C quit",
            app.session_rule_count
        );
    }
    app.reason.as_ref().map_or_else(
        || {
            let capture = capture_indicator(&app.debug_capture);
            let clear_hint = if app.connected {
                "C clear rules | "
            } else {
                ""
            };
            format!(
                "{clear_hint}{} · session rules: {}{}",
                sanitize(&app.status),
                app.session_rule_count,
                capture
            )
        },
        |reason| {
            reason.error.as_ref().map_or_else(
                || format!("Deny reason: {}", sanitize(&reason.value)),
                |error| format!("Deny reason: {} · {error}", sanitize(&reason.value)),
            )
        },
    )
}

fn render_footer(frame: &mut Frame<'_>, app: &App, area: Rect) {
    if app.clear_confirmation == ClearConfirmation::Hidden {
        frame.render_widget(
            Paragraph::new("Resize to confirm. n/Esc cancel | Ctrl-C quit")
                .wrap(Wrap { trim: false }),
            area,
        );
        return;
    }
    let message = footer_message(app);
    frame.render_widget(Paragraph::new(message).wrap(Wrap { trim: false }), area);
}

fn render_save_dialog(frame: &mut Frame<'_>, app: &App) {
    let Some(save) = &app.save else {
        return;
    };
    let area = centered_rect(72, 8, frame.area());
    let input = format!("File name or path: {}", sanitize(&save.value));
    let message = save.error.as_deref().unwrap_or("Enter save · Esc cancel");
    let dialog = Paragraph::new(vec![Line::from(input), Line::from(""), Line::from(message)])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Save session rules "),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(Clear, area);
    frame.render_widget(dialog, area);
    let cursor_x = area
        .x
        .saturating_add(1)
        .saturating_add(u16::try_from("File name or path: ".len()).unwrap_or(0))
        .saturating_add(u16::try_from(save.value.chars().count()).unwrap_or(u16::MAX));
    frame.set_cursor_position((cursor_x.min(area.right().saturating_sub(1)), area.y + 1));
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width.saturating_sub(2));
    let height = height.min(area.height.saturating_sub(2));
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
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

    use super::{App, ClearConfirmation, ReasonInput, handle_key, render};
    use crate::broker::{ApprovalId, ApprovalSummary};
    use crate::policy::RuleScope;

    fn approval() -> ApprovalSummary {
        ApprovalSummary {
            approval_id: "appr_0123456789abcdef".parse().unwrap(),
            capability_type: "command".to_owned(),
            summary: "a very long request summary that must visibly truncate".to_owned(),
            received_at: "2026-07-29T00:00:00Z".to_owned(),
            deadline: "2026-07-29T00:04:30Z".to_owned(),
        }
    }

    fn test_server(
        broker: &crate::broker::Broker,
        socket: &Path,
    ) -> tokio::task::JoinHandle<std::io::Result<()>> {
        tokio::spawn(crate::control::serve(
            tokio::net::UnixListener::bind(socket).unwrap(),
            crate::control::ControlContext {
                broker: broker.clone(),
                started_at: std::time::Instant::now(),
                webhook_listen: "127.0.0.1:0".to_owned(),
                max_pending: 64,
                max_per_session: 8,
                debug_capture: None,
            },
        ))
    }

    fn capability(id: &str, path: &str) -> crate::protocol::IncomingApproval {
        let value = serde_json::json!({"backend":"test", "request": {
            "capability_type":"capability", "request_id":id, "session_id":"tui",
            "child_pid":1, "path":path, "access":"Read"
        }});
        crate::protocol::parse_default_webhook_body(&serde_json::to_vec(&value).unwrap()).unwrap()
    }

    async fn keys(app: &mut App, codes: &[KeyCode]) {
        for code in codes {
            handle_key(app, KeyEvent::new(*code, KeyModifiers::NONE)).await;
        }
    }

    fn rendered(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render(frame, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    #[test]
    fn disconnect_clears_all_request_state() {
        let mut app = App::new(Path::new("/tmp/unreachable-control.sock"));
        app.connected = true;
        app.session_rule_count = 3;
        app.approvals.push(approval());
        app.selected = Some(0);
        app.detail_scroll = 42;
        app.show_detail_panel = true;
        app.reason = Some(ReasonInput {
            approval_id: "appr_0123456789abcdef".parse().unwrap(),
            value: "draft".to_owned(),
            error: None,
        });
        app.clear_confirmation = ClearConfirmation::Visible;
        app.disconnect();
        assert!(!app.connected);
        assert_eq!(app.session_rule_count, 0);
        assert!(app.approvals.is_empty());
        assert!(app.detail.is_none());
        assert_eq!(app.detail_scroll, 0);
        assert!(app.selected.is_none());
        assert!(app.reason.is_none());
        assert_eq!(app.clear_confirmation, ClearConfirmation::Closed);
    }

    #[tokio::test]
    async fn clear_rules_requires_confirmation_and_preserves_pending_requests() {
        use crate::broker::{Broker, BrokerConfig};
        use crate::control::{ControlClient, SessionRuleRequest};
        use crate::policy::RuleAction;

        let temporary = tempfile::tempdir().unwrap();
        let socket = temporary.path().join("control.sock");
        let broker = Broker::new(BrokerConfig::default()).unwrap();
        let server = test_server(&broker, &socket);
        let client = ControlClient::new(&socket);
        for (id, action) in [("allow", RuleAction::Allow), ("deny", RuleAction::Deny)] {
            let source = broker
                .submit(capability(id, &format!("/{id}")))
                .await
                .unwrap();
            client
                .remember(
                    &source.approval_id,
                    &SessionRuleRequest {
                        action,
                        scope: RuleScope::Path,
                        reason: None,
                        path: None,
                    },
                )
                .await
                .unwrap();
            source.wait().await;
        }
        let pending = broker
            .submit(capability("pending", "/pending"))
            .await
            .unwrap();
        let mut app = App::new(&socket);
        app.refresh().await;
        let original_id = app.selected_id();
        let original_deadline = app.detail.as_ref().unwrap().deadline.clone();
        assert_eq!(app.session_rule_count, 2);
        assert!(rendered(&mut app, 60, 24).contains("C clear rules"));

        for cancel in [KeyCode::Char('n'), KeyCode::Esc] {
            keys(&mut app, &[KeyCode::Char('C')]).await;
            app.refresh().await;
            assert_ne!(app.clear_confirmation, ClearConfirmation::Closed);
            assert!(rendered(&mut app, 60, 24).contains("Clear ALL 2"));
            keys(
                &mut app,
                &[
                    KeyCode::Enter,
                    KeyCode::Char('a'),
                    KeyCode::Char('d'),
                    KeyCode::Char('D'),
                    KeyCode::Char('r'),
                    KeyCode::Down,
                    KeyCode::Tab,
                ],
            )
            .await;
            assert_eq!(app.selected_id(), original_id);
            assert!(app.reason.is_none());
            assert!(app.editing.is_none());
            assert_eq!(broker.session_rule_count().await, 2);
            keys(&mut app, &[cancel]).await;
            assert_eq!(app.clear_confirmation, ClearConfirmation::Closed);
        }
        keys(&mut app, &[KeyCode::Char('C'), KeyCode::Char('y')]).await;
        assert_eq!(broker.session_rule_count().await, 2); // Not drawn yet.
        assert!(rendered(&mut app, 60, 24).contains("y confirm"));
        for modifiers in [
            KeyModifiers::CONTROL,
            KeyModifiers::ALT,
            KeyModifiers::SUPER,
        ] {
            handle_key(&mut app, KeyEvent::new(KeyCode::Char('y'), modifiers)).await;
        }
        for kind in [
            crossterm::event::KeyEventKind::Repeat,
            crossterm::event::KeyEventKind::Release,
        ] {
            handle_key(
                &mut app,
                KeyEvent::new_with_kind(KeyCode::Char('y'), KeyModifiers::NONE, kind),
            )
            .await;
        }
        assert_eq!(broker.session_rule_count().await, 2);
        keys(&mut app, &[KeyCode::Char('y')]).await;
        assert_eq!(app.clear_confirmation, ClearConfirmation::Closed);
        assert_eq!(app.session_rule_count, 0);
        assert_eq!(broker.session_rule_count().await, 0);
        assert_eq!(broker.pending_count().await, 1);
        app.refresh().await;
        assert_eq!(app.selected_id(), Some(pending.approval_id.clone()));
        assert_eq!(app.detail.as_ref().unwrap().deadline, original_deadline);
        assert_eq!(app.status, "Cleared 2 session rule(s).");
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn clear_rules_works_with_an_empty_queue_and_no_rules() {
        use crate::broker::{Broker, BrokerConfig};
        let temporary = tempfile::tempdir().unwrap();
        let socket = temporary.path().join("control.sock");
        let broker = Broker::new(BrokerConfig::default()).unwrap();
        let server = test_server(&broker, &socket);
        let mut app = App::new(&socket);
        app.refresh().await;
        assert!(app.approvals.is_empty());
        keys(&mut app, &[KeyCode::Char('C')]).await;
        assert!(rendered(&mut app, 60, 24).contains("Clear ALL 0"));
        keys(&mut app, &[KeyCode::Char('y')]).await;
        app.refresh().await;
        assert_eq!(app.status, "Cleared 0 session rule(s).");
        assert_eq!(app.clear_confirmation, ClearConfirmation::Closed);
        assert_eq!(broker.pending_count().await, 0);
        assert_eq!(broker.session_rule_count().await, 0);
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn clear_prompt_is_visible_without_pending_requests_and_safe_when_too_small() {
        let temporary = tempfile::tempdir().unwrap();
        let mut app = App::new(&temporary.path().join("missing.sock"));
        keys(&mut app, &[KeyCode::Char('C')]).await;
        assert_eq!(app.clear_confirmation, ClearConfirmation::Closed);
        app.connected = true;
        for modifiers in [
            KeyModifiers::CONTROL,
            KeyModifiers::ALT,
            KeyModifiers::SUPER,
        ] {
            handle_key(&mut app, KeyEvent::new(KeyCode::Char('C'), modifiers)).await;
        }
        for kind in [
            crossterm::event::KeyEventKind::Repeat,
            crossterm::event::KeyEventKind::Release,
        ] {
            handle_key(
                &mut app,
                KeyEvent::new_with_kind(KeyCode::Char('C'), KeyModifiers::SHIFT, kind),
            )
            .await;
        }
        assert_eq!(app.clear_confirmation, ClearConfirmation::Closed);
        assert!(rendered(&mut app, 60, 24).contains("C clear rules"));
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('C'), KeyModifiers::SHIFT),
        )
        .await;
        for (width, height) in [(120, 30), (80, 24), (60, 24), (36, 12)] {
            let text = rendered(&mut app, width, height);
            assert_eq!(app.clear_confirmation, ClearConfirmation::Visible);
            for expected in [
                "Clear ALL 0",
                "allow/deny",
                "daemon",
                "all clients",
                "Pending",
                "unchanged",
                "y confirm",
                "n/Esc cancel",
            ] {
                assert!(
                    text.contains(expected),
                    "missing {expected} at {width}x{height}"
                );
            }
        }
        assert!(rendered(&mut app, 40, 5).contains("Resize to confirm"));
        assert_eq!(app.clear_confirmation, ClearConfirmation::Hidden);
        keys(&mut app, &[KeyCode::Char('y')]).await;
        assert_eq!(app.clear_confirmation, ClearConfirmation::Hidden);
        assert!(
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
            )
            .await
        );
        keys(&mut app, &[KeyCode::Esc]).await;
        assert_eq!(app.clear_confirmation, ClearConfirmation::Closed);
        app.reason = Some(ReasonInput {
            approval_id: "appr_0123456789abcdef".parse().unwrap(),
            value: String::new(),
            error: None,
        });
        keys(&mut app, &[KeyCode::Char('C')]).await;
        assert_eq!(app.reason.as_ref().unwrap().value, "C");
        assert_eq!(app.clear_confirmation, ClearConfirmation::Closed);
    }

    #[tokio::test]
    async fn clear_failure_is_reported_and_disconnect_cancels_confirmation() {
        let temporary = tempfile::tempdir().unwrap();
        let mut app = App::new(&temporary.path().join("missing.sock"));
        app.connected = true;
        app.session_rule_count = 3;
        keys(&mut app, &[KeyCode::Char('C')]).await;
        rendered(&mut app, 60, 24);
        keys(&mut app, &[KeyCode::Char('y')]).await;
        assert_eq!(app.clear_confirmation, ClearConfirmation::Closed);
        assert_eq!(app.session_rule_count, 3);
        assert!(app.status.starts_with("Clear session rules failed:"));
        app.refresh().await;
        assert!(!app.connected);
        assert!(app.status.starts_with("Clear session rules failed:"));
        app.connected = true;
        keys(&mut app, &[KeyCode::Char('C')]).await;
        rendered(&mut app, 60, 24);
        app.refresh().await;
        assert_eq!(app.clear_confirmation, ClearConfirmation::Closed);
        app.connected = true;
        keys(&mut app, &[KeyCode::Char('y')]).await;
        assert_eq!(app.clear_confirmation, ClearConfirmation::Closed);
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
            terminal.draw(|frame| render(frame, &mut app)).unwrap();
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

    #[tokio::test]
    async fn rule_shortcuts_open_drafts_and_require_explicit_submission() {
        use crate::broker::{Broker, BrokerConfig, IngressOutcome};
        use crate::protocol::{WebhookDecision, parse_default_webhook_body};

        let temporary = tempfile::tempdir().unwrap();
        let socket = temporary.path().join("control.sock");
        let broker = Broker::new(BrokerConfig::default()).unwrap();
        let server = test_server(&broker, &socket);
        for key in ['p', 'P', 'A', 'r'] {
            let value = serde_json::json!({"backend":"test", "request": {
                "capability_type":"capability", "request_id":key.to_string(), "session_id":"tui",
                "child_pid":1, "path":"/work", "access":"Read"
            }});
            let incoming =
                || parse_default_webhook_body(&serde_json::to_vec(&value).unwrap()).unwrap();
            let submission = broker.submit(incoming()).await.unwrap();
            let mut app = App::new(&socket);
            app.refresh().await;
            assert_eq!(app.selected_id(), Some(submission.approval_id.clone()));
            for modifiers in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
                handle_key(&mut app, KeyEvent::new(KeyCode::Char(key), modifiers)).await;
            }
            handle_key(
                &mut app,
                KeyEvent::new_with_kind(
                    KeyCode::Char(key),
                    KeyModifiers::NONE,
                    crossterm::event::KeyEventKind::Repeat,
                ),
            )
            .await;
            assert_eq!(broker.session_rule_count().await, 0);
            assert_eq!(broker.pending_count().await, 1);
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE),
            )
            .await;
            assert!(app.editing.is_some());
            assert_eq!(broker.pending_count().await, 1);
            assert_eq!(broker.session_rule_count().await, 0);
            // Former form controls must never submit a rule.
            for code in [KeyCode::Enter, KeyCode::Tab, KeyCode::Tab] {
                handle_key(&mut app, KeyEvent::new(code, KeyModifiers::NONE)).await;
            }
            assert_eq!(broker.session_rule_count().await, 0);
            for width in [40, 60, 80, 100] {
                let rendered = rendered(&mut app, width, 30);
                assert!(rendered.contains("Rule scope"));
                assert!(rendered.contains("Source request"));
                assert!(rendered.contains("remember"));
            }
            let action = if key == 'A' || key == 'r' { 'a' } else { 'd' };
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(action), KeyModifiers::NONE),
            )
            .await;
            assert!(app.editing.is_none());
            let decision = submission.wait().await;
            assert_eq!(
                decision == WebhookDecision::Granted,
                key == 'A' || key == 'r'
            );
            app.refresh().await;
            assert_eq!(app.session_rule_count, 1);
            assert!(app.status.contains("remembered"));
            let probe = serde_json::json!({"backend":"test", "request": {
                "capability_type":"capability", "request_id":format!("{key}-probe"), "session_id":"tui",
                "child_pid":1, "path":"/work", "access":"Read"
            }});
            assert!(matches!(
                broker
                    .ingress(
                        parse_default_webhook_body(&serde_json::to_vec(&probe).unwrap()).unwrap()
                    )
                    .await
                    .unwrap(),
                IngressOutcome::Automatic(_)
            ));
            for width in [36, 100] {
                let rendered = rendered(&mut app, width, 14);
                assert!(rendered.contains("rules:"));
            }
            assert_eq!(app.client.clear_session_rules().await.unwrap().cleared, 1);
        }
        let command = parse_default_webhook_body(br#"{"backend":"x","request":{"capability_type":"command","request_id":"c","session_id":"s","command":"date","args":[],"caller":"test","intercept_rule":"test","child_pid":1}}"#).unwrap();
        let submission = broker.submit(command).await.unwrap();
        let mut app = App::new(&socket);
        app.refresh().await;
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('A'), KeyModifiers::NONE),
        )
        .await;
        app.refresh().await;
        assert!(app.status.contains("capability request"));
        assert_eq!(app.session_rule_count, 0);
        assert_eq!(app.selected_id(), Some(submission.approval_id.clone()));
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn file_request_can_create_project_tree_rule_without_mutating_source() {
        use crate::broker::{Broker, BrokerConfig, IngressOutcome};
        use crate::protocol::WebhookDecision;
        let temporary = tempfile::tempdir().unwrap();
        let socket = temporary.path().join("control.sock");
        let broker = Broker::new(BrokerConfig::default()).unwrap();
        let server = test_server(&broker, &socket);
        let submission = broker
            .submit(capability("source", "/path/to/project/src/main.rs"))
            .await
            .unwrap();
        let mut app = App::new(&socket);
        app.refresh().await;
        keys(
            &mut app,
            &[KeyCode::Char('r'), KeyCode::Left, KeyCode::Char('h')],
        )
        .await;
        assert_eq!(
            app.editing.as_ref().unwrap().editor.draft().path,
            "/path/to/project"
        );
        assert_eq!(
            app.editing.as_ref().unwrap().editor.draft().scope,
            RuleScope::Directory
        );
        assert!(app.editing.as_ref().unwrap().blocked().is_none());
        // A local wall-clock countdown is not the daemon's monotonic approval lease.
        app.editing.as_mut().unwrap().deadline = "1970-01-01T00:00:00Z".to_owned();
        assert!(app.editing.as_ref().unwrap().blocked().is_none());
        app.debug_capture = crate::control::DebugCaptureStatus::Failed {
            error_category: "io:Other".to_owned(),
        };
        for width in [40, 60, 80, 100] {
            let text = rendered(&mut app, width, 30);
            assert!(text.contains("Rule scope"));
            assert!(text.contains("a approve + remember"));
            assert!(text.contains("capture: failed"));
        }
        assert_eq!(
            app.editing.as_ref().unwrap().request,
            capability("source", "/path/to/project/src/main.rs").request
        );
        keys(&mut app, &[KeyCode::Char('a')]).await;
        assert_eq!(submission.wait().await, WebhookDecision::Granted);
        assert!(matches!(
            broker
                .ingress(capability("sibling", "/path/to/project/tests/test.rs"))
                .await
                .unwrap(),
            IngressOutcome::Automatic(WebhookDecision::Granted)
        ));
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn selector_resize_keeps_controls_visible_and_blocks_hidden_decisions() {
        use crate::broker::{Broker, BrokerConfig};
        let temporary = tempfile::tempdir().unwrap();
        let socket = temporary.path().join("control.sock");
        let broker = Broker::new(BrokerConfig::default()).unwrap();
        let server = test_server(&broker, &socket);
        let submission = broker
            .submit(capability("source", "/work/main.rs"))
            .await
            .unwrap();
        let mut app = App::new(&socket);
        app.refresh().await;
        keys(&mut app, &[KeyCode::Char('r')]).await;
        for (width, height) in [(120, 30), (80, 24), (60, 24), (40, 22)] {
            let text = rendered(&mut app, width, height);
            for expected in [
                "/work/main.rs",
                "Exact path",
                "Left/h",
                "Right/l",
                "Esc back",
                "a approve",
                "d deny",
                "Space",
            ] {
                assert!(
                    text.contains(expected),
                    "missing {expected} at {width}x{height}"
                );
            }
            keys(&mut app, &[KeyCode::Left, KeyCode::Left]).await;
            let text = rendered(&mut app, width, height);
            assert!(
                text.contains("Warning: all absolute paths"),
                "root warning at {width}x{height}"
            );
            keys(&mut app, &[KeyCode::Right, KeyCode::Right]).await;
        }
        for (width, height) in [(39, 24), (80, 21)] {
            assert!(rendered(&mut app, width, height).contains("need 40x22"));
            keys(
                &mut app,
                &[KeyCode::Char('a'), KeyCode::Char('d'), KeyCode::Left],
            )
            .await;
            assert_eq!(broker.session_rule_count().await, 0);
            assert_eq!(broker.pending_count().await, 1);
            assert_eq!(
                app.editing.as_ref().unwrap().editor.draft().path,
                "/work/main.rs"
            );
        }
        assert!(rendered(&mut app, 60, 24).contains("a approve"));
        // A transient submission error must not prevent an explicit retry.
        app.editing.as_mut().unwrap().error =
            Some("Session rule failed: transient error".to_owned());
        keys(&mut app, &[KeyCode::Char('d')]).await;
        assert!(app.editing.is_none());
        assert_eq!(broker.session_rule_count().await, 1);
        assert!(matches!(
            submission.wait().await,
            crate::protocol::WebhookDecision::Denied { .. }
        ));
        let next = broker
            .submit(capability("next", "/other/path"))
            .await
            .unwrap();
        app.refresh().await;
        for action in ['a', 'd', 'D'] {
            handle_key(
                &mut app,
                KeyEvent::new_with_kind(
                    KeyCode::Char(action),
                    KeyModifiers::NONE,
                    crossterm::event::KeyEventKind::Repeat,
                ),
            )
            .await;
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(action), KeyModifiers::ALT),
            )
            .await;
        }
        assert_eq!(app.selected_id(), Some(next.approval_id.clone()));
        assert_eq!(broker.pending_count().await, 1);
        assert!(app.reason.is_none());
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn draft_is_cancelable_and_never_rebinds_when_source_disappears() {
        use crate::broker::{Broker, BrokerConfig, ShowApproval};
        use crate::protocol::WebhookDecision;
        let temporary = tempfile::tempdir().unwrap();
        let socket = temporary.path().join("control.sock");
        let broker = Broker::new(BrokerConfig::default()).unwrap();
        let server = test_server(&broker, &socket);
        let source = broker
            .submit(capability("source", "/work/\u{1b}[31m/main.rs"))
            .await
            .unwrap();
        let mut app = App::new(&socket);
        app.refresh().await;
        keys(&mut app, &[KeyCode::Char('r'), KeyCode::Char('C')]).await;
        assert_eq!(app.clear_confirmation, ClearConfirmation::Closed);
        assert_eq!(
            app.editing.as_ref().unwrap().editor.draft().path,
            "/work/\u{1b}[31m/main.rs"
        );
        keys(&mut app, &[KeyCode::Esc]).await;
        assert!(app.editing.is_none());
        assert_eq!(broker.session_rule_count().await, 0);
        keys(&mut app, &[KeyCode::Char('r')]).await;
        let next = broker
            .submit(capability("next", "/work/other.rs"))
            .await
            .unwrap();
        broker
            .decide(&source.approval_id, WebhookDecision::Granted)
            .await
            .unwrap();
        source.wait().await;
        app.refresh().await;
        assert_eq!(app.selected_id(), None);
        assert!(!app.editing.as_ref().unwrap().available);
        keys(&mut app, &[KeyCode::Char('a'), KeyCode::Char('d')]).await;
        assert!(
            app.editing
                .as_ref()
                .unwrap()
                .blocked()
                .unwrap()
                .contains("no longer pending")
        );
        assert_eq!(broker.session_rule_count().await, 0);
        assert!(matches!(
            broker.show(&next.approval_id).await.unwrap(),
            ShowApproval::Pending(_)
        ));
        app.disconnect();
        assert!(app.editing.is_none());
        app.refresh().await;
        assert!(app.editing.is_none());
        assert_eq!(app.selected_id(), Some(next.approval_id.clone()));
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn save_flow_sets_status_deadline_on_success_and_failure() {
        use crate::broker::{Broker, BrokerConfig};
        use crate::policy::{RuleAction, RuleDraft, RuleScope};
        use crate::protocol::AccessMode;
        use std::time::{Duration, Instant};

        let temporary = tempfile::tempdir().unwrap();
        let socket = temporary.path().join("control.sock");
        let broker = Broker::new(BrokerConfig::default()).unwrap();
        let server = test_server(&broker, &socket);
        let mut app = App::new(&socket);
        app.refresh().await;

        // Failure paths must set the status deadline like the success path.
        let before = Instant::now();
        app.save_session_rules("/definitely/not/created/policy.toml")
            .await;
        assert_eq!(app.status, "Save rules failed: no session rules");
        assert!(app.status_until >= before + Duration::from_secs(3));

        app.client
            .replace_session_rules(&[RuleDraft {
                action: RuleAction::Allow,
                path: "/work".to_owned(),
                scope: RuleScope::Directory,
                access: AccessMode::Read,
            }])
            .await
            .unwrap();
        let target = temporary.path().join("saved.toml");
        app.save_session_rules(target.to_str().unwrap()).await;
        assert!(
            app.status.contains("Saved session rules to"),
            "{}",
            app.status
        );
        let contents = std::fs::read_to_string(&target).unwrap();
        assert!(contents.contains("[[rules]]"));
        assert!(contents.contains("path = \"/work\""));
        assert!(contents.contains("access = \"read\""));

        // Existing files are never overwritten, and the failure still sets
        // the deadline while leaving the file untouched.
        let before = Instant::now();
        app.save_session_rules(target.to_str().unwrap()).await;
        assert!(app.status.contains("Save rules failed"), "{}", app.status);
        assert!(app.status_until >= before + Duration::from_secs(3));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), contents);
        server.abort();
        let _ = server.await;
    }
}
