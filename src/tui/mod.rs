//! Terminal UI: ratatui-based interface for browsing, editing, and copying
//! credentials.
//!
//! Layout:
//! ```text
//! ┌─ DevCred ──────────────────────────────────────────────┐
//! │ [search] filter: env=prod project=web-app   12 items   │
//! ├────────────────────────────────────────────────────────┤
//! │  ID  NAME              KIND      ENV      PROJECT      │
//! │   1  github-personal   github    prod     web-app      │
//! │   2  pypi-upload       pypi      prod     web-app  ←   │
//! ├────────────────────────────────────────────────────────┤
//! │ Detail: pypi-upload · env_var=TWINE_PASSWORD           │
//! │ /search  e:env  p:project  n:new  enter:copy  ?:help   │
//! └────────────────────────────────────────────────────────┘
//! ```

mod ui;

use crate::credential::{self, CredentialKind};
use crate::db::{CredentialRecord, DecryptedCredential, Vault};
use crate::clipboard;
use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Run the TUI until the user quits. Restores the terminal on exit.
pub fn run(vault: Vault, vault_path: PathBuf) -> Result<()> {
    enable_raw_mode().context("enabling raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("entering alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("creating terminal")?;
    terminal.clear().ok();

    let mut app = App::new(vault, vault_path);
    let result = app_loop(&mut terminal, &mut app);

    // Restore terminal regardless of outcome.
    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();

    result
}

fn app_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> Result<()> {
    let tick = Duration::from_millis(120);
    loop {
        terminal.draw(|f| ui::draw(f, app))?;
        if event::poll(tick)? {
            match event::read()? {
                // On Windows, crossterm reports both Press and Release events.
                // Only handle Press to avoid doubled characters.
                Event::Key(k) if k.kind == KeyEventKind::Press => {
                    if let Some(action) = app.handle_key(k) {
                        match action {
                            Action::Quit => return Ok(()),
                            Action::CopyAndNotify { name, secret, secs } => {
                                if let Err(e) = clipboard::copy_and_clear_after(&secret, secs) {
                                    app.toast_info(format!("clipboard error: {e}"));
                                } else {
                                    app.toast_ok_msg(format!(
                                        "Copied `{}` — clears in {}s.",
                                        name, secs
                                    ));
                                }
                            }
                        }
                    }
                }
                Event::Resize(_, _) => { /* ratatui handles on next draw */ }
                _ => {}
            }
        }
        app.refresh_toast();
    }
}

/// High-level action returned by key handling.
pub enum Action {
    Quit,
    /// Copy the given secret and show a toast; auto-clear after N seconds.
    CopyAndNotify { name: String, secret: String, secs: u64 },
}

/// Map a Shift+key press to a field index for quick-copy in the detail view.
/// Shift+1..9 → 0..8, Shift+0 → 9, Shift+A..Z → 10..35.
/// Also matches the US-keyboard shifted symbols (! @ # $ % ^ & * ( )) as fallback.
fn shift_field_index(k: &KeyEvent) -> Option<usize> {
    match k.code {
        KeyCode::Char('1') | KeyCode::Char('!') => Some(0),
        KeyCode::Char('2') | KeyCode::Char('@') => Some(1),
        KeyCode::Char('3') | KeyCode::Char('#') => Some(2),
        KeyCode::Char('4') | KeyCode::Char('$') => Some(3),
        KeyCode::Char('5') | KeyCode::Char('%') => Some(4),
        KeyCode::Char('6') | KeyCode::Char('^') => Some(5),
        KeyCode::Char('7') | KeyCode::Char('&') => Some(6),
        KeyCode::Char('8') | KeyCode::Char('*') => Some(7),
        KeyCode::Char('9') | KeyCode::Char('(') => Some(8),
        KeyCode::Char('0') | KeyCode::Char(')') => Some(9),
        KeyCode::Char(c) if c.is_ascii_uppercase() => Some(10 + (c as u8 - b'A') as usize),
        _ => None,
    }
}

/// Which form field is being edited in Add/Edit mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormField {
    Name,
    Secret,
    /// Credential type selector (cycled with arrows, not typed).
    Kind,
    Env,
    Project,
    EnvVar,
    Notes,
    /// Key of the i-th custom field.
    CustomKey(usize),
    /// Value of the i-th custom field.
    CustomValue(usize),
    /// Masked toggle of the i-th custom field.
    CustomMasked(usize),
    /// "[+ Add custom field]" button.
    AddField,
}

impl FormField {
    fn next(self, form: &Form) -> Self {
        match self {
            FormField::Name => FormField::Secret,
            FormField::Secret => FormField::Kind,
            FormField::Kind => FormField::Env,
            FormField::Env => FormField::Project,
            FormField::Project => FormField::EnvVar,
            FormField::EnvVar => FormField::Notes,
            FormField::Notes => first_custom_or_add(form),
            FormField::CustomKey(i) => FormField::CustomValue(i),
            FormField::CustomValue(i) => FormField::CustomMasked(i),
            FormField::CustomMasked(i) => {
                if i + 1 < form.custom_fields.len() {
                    FormField::CustomKey(i + 1)
                } else {
                    FormField::AddField
                }
            }
            FormField::AddField => FormField::Name,
        }
    }
    fn prev(self, form: &Form) -> Self {
        match self {
            FormField::Name => FormField::AddField,
            FormField::Secret => FormField::Name,
            FormField::Kind => FormField::Secret,
            FormField::Env => FormField::Kind,
            FormField::Project => FormField::Env,
            FormField::EnvVar => FormField::Project,
            FormField::Notes => FormField::EnvVar,
            FormField::CustomKey(0) => FormField::Notes,
            FormField::CustomKey(i) => FormField::CustomMasked(i - 1),
            FormField::CustomValue(i) => FormField::CustomKey(i),
            FormField::CustomMasked(i) => FormField::CustomValue(i),
            FormField::AddField => {
                if form.custom_fields.is_empty() {
                    FormField::Notes
                } else {
                    FormField::CustomMasked(form.custom_fields.len() - 1)
                }
            }
        }
    }
    fn label(self) -> &'static str {
        match self {
            FormField::Name => "Name",
            FormField::Secret => "Secret",
            FormField::Kind => "Kind",
            FormField::Env => "Environment",
            FormField::Project => "Project",
            FormField::EnvVar => "Env Var",
            FormField::Notes => "Notes",
            FormField::CustomKey(_) => "Field Key",
            FormField::CustomValue(_) => "Field Value",
            FormField::CustomMasked(_) => "Masked",
            FormField::AddField => "Add Field",
        }
    }
    /// One-line hint shown in the form for the focused field.
    fn description(self) -> &'static str {
        match self {
            FormField::Name => "Display name for this credential, e.g. github-personal",
            FormField::Secret => "The token/key. Type is auto-detected from its prefix",
            FormField::Kind => "Credential type. ←/→ to override auto-detection",
            FormField::Env => "Optional tag for filtering: prod / staging / dev / personal",
            FormField::Project => "Optional project group, e.g. web-app (orthogonal to env)",
            FormField::EnvVar => "Variable name used by `inject`, e.g. GITHUB_TOKEN (auto-filled)",
            FormField::Notes => "Free-form notes (optional)",
            FormField::CustomKey(_) => "Custom field name",
            FormField::CustomValue(_) => "Custom field value",
            FormField::CustomMasked(_) => "Space toggles masked display. Backspace removes this field",
            FormField::AddField => "Enter to add a new custom field",
        }
    }
}

/// First custom field key, or the AddField button if none exist.
fn first_custom_or_add(form: &Form) -> FormField {
    if form.custom_fields.is_empty() {
        FormField::AddField
    } else {
        FormField::CustomKey(0)
    }
}

/// Active screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    List,
    Search,
    FilterEnv,
    FilterProject,
    Add,
    Edit,
    Detail,
    ConfirmDelete,
    RevealPrompt,
    Help,
    Settings,
}

/// Which settings tab is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    Password,
    Tokens,
    Info,
}

/// Which pane has keyboard focus in the main list view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// Left pane: category/kind sidebar.
    Category,
    /// Right pane: credential list.
    List,
}

/// A credential row in the filtered, sorted view.
#[derive(Debug, Clone)]
pub struct Row {
    pub rec: CredentialRecord,
    pub score: i64,
}

/// One user-defined field in the form.
#[derive(Debug, Clone, Default)]
pub struct CustomFieldForm {
    pub key: String,
    pub value: String,
    pub masked: bool,
}

/// Form state for Add/Edit.
#[derive(Debug, Clone, Default)]
pub struct Form {
    pub name: String,
    pub secret: String,
    /// Credential kind (auto-detected from `secret`, or manually overridden).
    pub kind: CredentialKind,
    /// True once the user has manually cycled `kind`.
    pub kind_manual: bool,
    pub env: String,
    pub project: String,
    pub env_var: String,
    pub notes: String,
    pub field: FormField,
    /// Detection result for the current secret (live feedback).
    pub detection: Option<credential::Detection>,
    /// When editing, the id being edited.
    pub editing_id: Option<i64>,
    /// User-defined custom fields.
    pub custom_fields: Vec<CustomFieldForm>,
    /// Cursor position (char index) within the currently-focused text field.
    pub cursor: usize,
}

impl Form {
    fn new() -> Self {
        Self {
            field: FormField::Name,
            ..Default::default()
        }
    }

    // fn current_field(&self) -> &str {
    //     match self.field {
    //         FormField::Name => &self.name,
    //         FormField::Secret => &self.secret,
    //         FormField::Kind => "",
    //         FormField::Env => &self.env,
    //         FormField::Project => &self.project,
    //         FormField::EnvVar => &self.env_var,
    //         FormField::Notes => &self.notes,
    //         FormField::CustomKey(i) => self.custom_fields.get(i).map(|f| f.key.as_str()).unwrap_or(""),
    //         FormField::CustomValue(i) => self.custom_fields.get(i).map(|f| f.value.as_str()).unwrap_or(""),
    //         FormField::CustomMasked(_) => "",
    //         FormField::AddField => "",
    //     }
    // }

    fn current_field_mut(&mut self) -> Option<&mut String> {
        match self.field {
            FormField::Name => Some(&mut self.name),
            FormField::Secret => Some(&mut self.secret),
            FormField::Kind => None,
            FormField::Env => Some(&mut self.env),
            FormField::Project => Some(&mut self.project),
            FormField::EnvVar => Some(&mut self.env_var),
            FormField::Notes => Some(&mut self.notes),
            FormField::CustomKey(i) => self.custom_fields.get_mut(i).map(|f| &mut f.key),
            FormField::CustomValue(i) => self.custom_fields.get_mut(i).map(|f| &mut f.value),
            FormField::CustomMasked(_) => None,
            FormField::AddField => None,
        }
    }

    fn recompute_detection(&mut self) {
        if self.secret.is_empty() {
            self.detection = None;
            if !self.kind_manual {
                self.kind = CredentialKind::Generic;
            }
        } else {
            let d = credential::detect(&self.secret);
            if !self.kind_manual {
                self.kind = d.kind.clone();
            }
            self.detection = Some(d);
        }
        if self.env_var.is_empty() {
            self.env_var = self.kind.env_var().to_string();
        }
    }

    /// Set cursor to the end of the current field's text.
    fn sync_cursor_to_end(&mut self) {
        self.cursor = self
            .current_field_mut()
            .map(|f| f.chars().count())
            .unwrap_or(0);
    }

    /// Insert a character at the cursor position.
    fn insert_char(&mut self, c: char) {
        match self.field {
            FormField::Name => text_insert(&mut self.name, &mut self.cursor, c),
            FormField::Secret => text_insert(&mut self.secret, &mut self.cursor, c),
            FormField::Kind => {}
            FormField::Env => text_insert(&mut self.env, &mut self.cursor, c),
            FormField::Project => text_insert(&mut self.project, &mut self.cursor, c),
            FormField::EnvVar => text_insert(&mut self.env_var, &mut self.cursor, c),
            FormField::Notes => text_insert(&mut self.notes, &mut self.cursor, c),
            FormField::CustomKey(i) => {
                if let Some(f) = self.custom_fields.get_mut(i) {
                    text_insert(&mut f.key, &mut self.cursor, c);
                }
            }
            FormField::CustomValue(i) => {
                if let Some(f) = self.custom_fields.get_mut(i) {
                    text_insert(&mut f.value, &mut self.cursor, c);
                }
            }
            FormField::CustomMasked(_) | FormField::AddField => {}
        }
    }

    /// Delete the character before the cursor.
    fn delete_before_cursor(&mut self) {
        match self.field {
            FormField::Name => text_delete_before(&mut self.name, &mut self.cursor),
            FormField::Secret => text_delete_before(&mut self.secret, &mut self.cursor),
            FormField::Kind => {}
            FormField::Env => text_delete_before(&mut self.env, &mut self.cursor),
            FormField::Project => text_delete_before(&mut self.project, &mut self.cursor),
            FormField::EnvVar => text_delete_before(&mut self.env_var, &mut self.cursor),
            FormField::Notes => text_delete_before(&mut self.notes, &mut self.cursor),
            FormField::CustomKey(i) => {
                if let Some(f) = self.custom_fields.get_mut(i) {
                    text_delete_before(&mut f.key, &mut self.cursor);
                }
            }
            FormField::CustomValue(i) => {
                if let Some(f) = self.custom_fields.get_mut(i) {
                    text_delete_before(&mut f.value, &mut self.cursor);
                }
            }
            FormField::CustomMasked(_) | FormField::AddField => {}
        }
    }

    /// Move cursor left by one char (clamped at 0).
    fn cursor_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Move cursor right by one char (clamped at text end).
    fn cursor_right(&mut self) {
        let max = match self.field {
            FormField::Name => self.name.chars().count(),
            FormField::Secret => self.secret.chars().count(),
            FormField::Kind => 0,
            FormField::Env => self.env.chars().count(),
            FormField::Project => self.project.chars().count(),
            FormField::EnvVar => self.env_var.chars().count(),
            FormField::Notes => self.notes.chars().count(),
            FormField::CustomKey(i) => self.custom_fields.get(i).map(|f| f.key.chars().count()).unwrap_or(0),
            FormField::CustomValue(i) => self.custom_fields.get(i).map(|f| f.value.chars().count()).unwrap_or(0),
            FormField::CustomMasked(_) | FormField::AddField => 0,
        };
        if self.cursor < max {
            self.cursor += 1;
        }
    }
}

/// Insert a character at `cursor` within `text`, advancing the cursor.
fn text_insert(text: &mut String, cursor: &mut usize, c: char) {
    let byte_idx = text
        .char_indices()
        .nth(*cursor)
        .map(|(i, _)| i)
        .unwrap_or(text.len());
    text.insert(byte_idx, c);
    *cursor += 1;
}

/// Delete the character before `cursor` within `text`, retreating the cursor.
fn text_delete_before(text: &mut String, cursor: &mut usize) {
    if *cursor > 0 {
        let prev = *cursor - 1;
        let byte_idx = text
            .char_indices()
            .nth(prev)
            .map(|(i, _)| i)
            .unwrap_or(0);
        let ch_len = text[byte_idx..]
            .chars()
            .next()
            .map(|c| c.len_utf8())
            .unwrap_or(0);
        text.drain(byte_idx..byte_idx + ch_len);
        *cursor = prev;
    }
}

impl Default for FormField {
    fn default() -> Self {
        FormField::Name
    }
}

/// The full application state.
pub struct App {
    pub vault: Vault,
    pub mode: Mode,
    pub prev_mode: Mode,
    /// Which pane is focused in the list view.
    pub focus: Focus,
    /// All records (refreshed from the vault).
    pub all: Vec<CredentialRecord>,
    /// Filtered + fuzzy-scored rows currently displayed.
    pub rows: Vec<Row>,
    pub selected: usize,
    /// Search query (empty = show all).
    pub search: String,
    pub env_filter: String,
    pub project_filter: String,
    /// Active category filter (None = All).
    pub category_filter: Option<CredentialKind>,
    /// Selected index in the category sidebar.
    pub category_sel: usize,
    /// (kind, count) pairs for the category sidebar, sorted by label.
    pub categories: Vec<(CredentialKind, usize)>,
    /// Available environments / projects for the filter pickers.
    pub envs: Vec<String>,
    pub projects: Vec<String>,
    pub env_picker_sel: usize,
    pub project_picker_sel: usize,
    pub form: Form,
    /// Cached decrypted secret for the detail view + copy.
    pub detail: Option<DecryptedCredential>,
    /// Transient status message.
    pub toast: String,
    pub toast_at: Instant,
    /// Whether the toast represents a success (green) vs error/info.
    pub toast_ok: bool,
    /// How long the toast should remain visible before auto-clearing.
    pub toast_duration: Duration,
    pub show_secret_in_detail: bool,
    /// Selected field index in detail view (None = nothing selected, 0 = Secret, 1+ = custom fields).
    pub detail_field_sel: Option<usize>,
    /// Field index that was just copied (for green flash feedback).
    pub copied_field_idx: Option<usize>,
    pub copied_at: Instant,
    /// True when a copy happened in the list view (for green row flash).
    pub copied_in_list: bool,
    /// Vertical scroll offset for the detail popup (when content overflows).
    pub detail_scroll: u16,
    /// Buffer for the reveal-password prompt.
    pub reveal_password: String,
    /// When true, the password prompt is gating an edit (not a reveal).
    pub pending_edit: bool,
    /// Cursor position within the search box.
    pub search_cursor: usize,
    /// Cursor position within the reveal-password prompt.
    pub reveal_cursor: usize,
    /// Path to the vault file (for settings display).
    pub vault_path: PathBuf,
    /// Settings tab.
    pub settings_tab: SettingsTab,
    /// New password input (settings → change password).
    pub settings_new_pw: String,
    /// Confirm password input.
    pub settings_confirm_pw: String,
    /// Which password field is focused: 0 = new, 1 = confirm.
    pub settings_pw_field: usize,
    /// Cursor in the password fields.
    pub settings_pw_cursor: usize,
    /// Cached token list for the settings → tokens tab.
    pub settings_tokens: Vec<crate::db::TokenInfo>,
    /// Selected token index in the tokens list.
    pub settings_token_sel: usize,
    /// Label input for creating a new token.
    pub settings_token_label: String,
    /// Cursor in the token label input.
    pub settings_token_label_cursor: usize,
    /// Whether the token label input is focused (vs the token list).
    pub settings_token_creating: bool,
}

impl App {
    fn new(vault: Vault, vault_path: PathBuf) -> Self {
        let mut app = App {
            vault,
            mode: Mode::List,
            prev_mode: Mode::List,
            focus: Focus::List,
            all: Vec::new(),
            rows: Vec::new(),
            selected: 0,
            search: String::new(),
            env_filter: String::new(),
            project_filter: String::new(),
            category_filter: None,
            category_sel: 0,
            categories: Vec::new(),
            envs: Vec::new(),
            projects: Vec::new(),
            env_picker_sel: 0,
            project_picker_sel: 0,
            form: Form::new(),
            detail: None,
            toast: String::new(),
            toast_at: Instant::now(),
            toast_ok: false,
            toast_duration: Duration::from_millis(1500),
            show_secret_in_detail: false,
            detail_field_sel: None,
            copied_field_idx: None,
            copied_at: Instant::now(),
            copied_in_list: false,
            detail_scroll: 0,
            reveal_password: String::new(),
            pending_edit: false,
            search_cursor: 0,
            reveal_cursor: 0,
            vault_path,
            settings_tab: SettingsTab::Password,
            settings_new_pw: String::new(),
            settings_confirm_pw: String::new(),
            settings_pw_field: 0,
            settings_pw_cursor: 0,
            settings_tokens: Vec::new(),
            settings_token_sel: 0,
            settings_token_label: String::new(),
            settings_token_label_cursor: 0,
            settings_token_creating: false,
        };
        app.reload();
        app
    }

    fn reload(&mut self) {
        self.all = self.vault.list(None, None).unwrap_or_default();
        self.envs = self.vault.environments().unwrap_or_default();
        self.projects = self.vault.projects().unwrap_or_default();
        self.recompute_categories();
        self.recompute_rows();
    }

    /// Build the category list from `all`, counting per kind.
    fn recompute_categories(&mut self) {
        use std::collections::BTreeMap;
        let mut counts: BTreeMap<String, (CredentialKind, usize)> = BTreeMap::new();
        for r in &self.all {
            let kind = r.kind_enum();
            counts
                .entry(kind.label().to_string())
                .and_modify(|(_, c)| *c += 1)
                .or_insert((kind, 1));
        }
        self.categories = counts.into_values().collect();
        // Keep category_sel in bounds; index 0 = "All".
        let max = self.categories.len();
        if self.category_sel > max {
            self.category_sel = max;
        }
    }

    fn recompute_rows(&mut self) {
        let matcher = SkimMatcherV2::default();
        let q = self.search.trim().to_lowercase();
        let env = self.env_filter.trim();
        let proj = self.project_filter.trim();
        let cat = self.category_filter.clone();

        let mut rows: Vec<Row> = self
            .all
            .iter()
            .filter(|r| env.is_empty() || env == "*" || r.environment == env)
            .filter(|r| proj.is_empty() || proj == "*" || r.project == proj)
            .filter(|r| match &cat {
                None => true,
                Some(k) => &r.kind_enum() == k,
            })
            .filter_map(|r| {
                if q.is_empty() {
                    return Some(Row {
                        rec: r.clone(),
                        score: 0,
                    });
                }
                let hay = format!("{} {} {} {} {}", r.name, r.kind, r.environment, r.project, r.env_var).to_lowercase();
                match matcher.fuzzy_match(&hay, &q) {
                    Some(score) => Some(Row {
                        rec: r.clone(),
                        score,
                    }),
                    None => None,
                }
            })
            .collect();
        rows.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.rec.name.to_lowercase().cmp(&b.rec.name.to_lowercase())));
        if self.selected >= rows.len() {
            self.selected = rows.len().saturating_sub(1);
        }
        self.rows = rows;
    }

    fn refresh_toast(&mut self) {
        if !self.toast.is_empty() && self.toast_at.elapsed() > self.toast_duration {
            self.toast.clear();
            self.toast_ok = false;
        }
    }

    /// Set a success toast (rendered green) with default duration.
    fn toast_ok_msg(&mut self, msg: impl Into<String>) {
        self.toast = msg.into();
        self.toast_at = Instant::now();
        self.toast_ok = true;
        self.toast_duration = Duration::from_millis(1500);
    }

    /// Set a success toast with custom duration.
    fn toast_ok_msg_for(&mut self, msg: impl Into<String>, dur: Duration) {
        self.toast = msg.into();
        self.toast_at = Instant::now();
        self.toast_ok = true;
        self.toast_duration = dur;
    }

    /// Set an error/info toast (rendered blue/red).
    fn toast_info(&mut self, msg: impl Into<String>) {
        self.toast = msg.into();
        self.toast_at = Instant::now();
        self.toast_ok = false;
        self.toast_duration = Duration::from_millis(1500);
    }

    fn selected_record(&self) -> Option<&CredentialRecord> {
        self.rows.get(self.selected).map(|r| &r.rec)
    }

    fn move_selection(&mut self, delta: i32) {
        if self.rows.is_empty() {
            return;
        }
        let len = self.rows.len() as i32;
        let mut idx = self.selected as i32 + delta;
        if idx < 0 {
            idx = len - 1;
        } else if idx >= len {
            idx = 0;
        }
        self.selected = idx as usize;
    }

    fn handle_key(&mut self, k: KeyEvent) -> Option<Action> {
        // Global: Ctrl+C always quits.
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            return Some(Action::Quit);
        }
        match self.mode {
            Mode::List => self.handle_list(k),
            Mode::Search => self.handle_search(k),
            Mode::FilterEnv => self.handle_env_picker(k),
            Mode::FilterProject => self.handle_project_picker(k),
            Mode::Add | Mode::Edit => self.handle_form(k),
            Mode::Detail => self.handle_detail(k),
            Mode::ConfirmDelete => self.handle_confirm_delete(k),
            Mode::RevealPrompt => self.handle_reveal_prompt(k),
            Mode::Help => self.handle_help(k),
            Mode::Settings => self.handle_settings(k),
        }
    }

    fn handle_list(&mut self, k: KeyEvent) -> Option<Action> {
        // Tab switches focus between the category sidebar and the credential list.
        if k.code == KeyCode::Tab {
            self.focus = match self.focus {
                Focus::Category => Focus::List,
                Focus::List => Focus::Category,
            };
            return None;
        }

        // When the category sidebar is focused, only navigation + quit work.
        if self.focus == Focus::Category {
            return self.handle_category(k);
        }

        match k.code {
            KeyCode::Char('q') | KeyCode::Esc => return Some(Action::Quit),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::PageDown => self.move_selection(10),
            KeyCode::PageUp => self.move_selection(-10),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = self.rows.len().saturating_sub(1),
            KeyCode::Char('/') => {
                self.prev_mode = self.mode;
                self.mode = Mode::Search;
                self.search.clear();
            }
            KeyCode::Char('e') => {
                self.prev_mode = self.mode;
                self.mode = Mode::FilterEnv;
                self.env_picker_sel = 0;
            }
            KeyCode::Char('p') => {
                self.prev_mode = self.mode;
                self.mode = Mode::FilterProject;
                self.project_picker_sel = 0;
            }
            KeyCode::Char('n') => {
                if !self.deny_basic() {
                    self.start_add();
                }
            }
            KeyCode::Char('?') => {
                self.prev_mode = self.mode;
                self.mode = Mode::Help;
            }
            KeyCode::Char('s') => {
                self.open_settings();
            }
            KeyCode::Enter | KeyCode::Char('c') => return self.copy_selected(),
            KeyCode::Char('d') => {
                if self.deny_basic() {
                    // blocked
                } else if self.selected_record().is_some() {
                    self.prev_mode = self.mode;
                    self.mode = Mode::ConfirmDelete;
                }
            }
            KeyCode::Char('r') => {
                if !self.deny_basic() {
                    self.start_edit();
                }
            }
            KeyCode::Char('i') | KeyCode::Char(' ') => {
                if let Some(rec) = self.selected_record().cloned() {
                    self.detail = self.vault.decrypt(&rec).ok();
                    self.show_secret_in_detail = false;
                    self.detail_field_sel = None;
                    self.detail_scroll = 0;
                    self.prev_mode = self.mode;
                    self.mode = Mode::Detail;
                }
            }
            _ => {}
        }
        None
    }

    /// Key handler for the left category sidebar.
    /// Supports navigation, quit, search, filters, help, and new credential.
    /// Does NOT support i/d/r/c (credential-specific actions).
    fn handle_category(&mut self, k: KeyEvent) -> Option<Action> {
        // index 0 = "All", 1..=len = specific kinds.
        let max = self.categories.len(); // "All" is at index 0, kinds are 1..=max
        match k.code {
            KeyCode::Char('q') | KeyCode::Esc => return Some(Action::Quit),
            KeyCode::Up | KeyCode::Char('k') => {
                if self.category_sel > 0 {
                    self.category_sel -= 1;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.category_sel < max {
                    self.category_sel += 1;
                }
            }
            KeyCode::Home => self.category_sel = 0,
            KeyCode::End => self.category_sel = max,
            KeyCode::Char('/') => {
                self.apply_category_selection();
                self.prev_mode = self.mode;
                self.mode = Mode::Search;
                self.search.clear();
                return None;
            }
            KeyCode::Enter => {
                // Switch focus to the credential list.
                self.apply_category_selection();
                self.focus = Focus::List;
                return None;
            }
            KeyCode::Char(' ') => {
                self.apply_category_selection();
            }
            // Common keys that also work from the category sidebar:
            KeyCode::Char('e') => {
                self.apply_category_selection();
                self.prev_mode = self.mode;
                self.mode = Mode::FilterEnv;
                self.env_picker_sel = 0;
                return None;
            }
            KeyCode::Char('p') => {
                self.apply_category_selection();
                self.prev_mode = self.mode;
                self.mode = Mode::FilterProject;
                self.project_picker_sel = 0;
                return None;
            }
            KeyCode::Char('?') => {
                self.prev_mode = self.mode;
                self.mode = Mode::Help;
                return None;
            }
            KeyCode::Char('n') => {
                if !self.deny_basic() {
                    self.start_add();
                }
                return None;
            }
            KeyCode::Char('s') => {
                self.open_settings();
                return None;
            }
            // i/d/r/c are intentionally excluded (credential-specific).
            _ => {}
        }
        // Apply filter live as the cursor moves.
        self.apply_category_selection();
        None
    }

    /// Set `category_filter` based on `category_sel` (0 = All).
    fn apply_category_selection(&mut self) {
        if self.category_sel == 0 {
            self.category_filter = None;
        } else {
            self.category_filter = self
                .categories
                .get(self.category_sel - 1)
                .map(|(k, _)| k.clone());
        }
        self.recompute_rows();
    }

    fn handle_search(&mut self, k: KeyEvent) -> Option<Action> {
        match k.code {
            KeyCode::Esc => {
                self.search.clear();
                self.search_cursor = 0;
                self.recompute_rows();
                self.mode = Mode::List;
            }
            KeyCode::Enter => {
                self.recompute_rows();
                self.mode = Mode::List;
            }
            KeyCode::Left => {
                self.search_cursor = self.search_cursor.saturating_sub(1);
            }
            KeyCode::Right => {
                let max = self.search.chars().count();
                if self.search_cursor < max {
                    self.search_cursor += 1;
                }
            }
            KeyCode::Backspace => {
                text_delete_before(&mut self.search, &mut self.search_cursor);
                self.recompute_rows();
            }
            KeyCode::Char(c) => {
                text_insert(&mut self.search, &mut self.search_cursor, c);
                self.recompute_rows();
            }
            _ => {}
        }
        None
    }

    fn handle_env_picker(&mut self, k: KeyEvent) -> Option<Action> {
        let options_count = self.envs.len() + 1; // +1 for "(all)"
        match k.code {
            KeyCode::Esc => self.mode = Mode::List,
            KeyCode::Up | KeyCode::Char('k') => {
                if self.env_picker_sel > 0 {
                    self.env_picker_sel -= 1;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.env_picker_sel + 1 < options_count {
                    self.env_picker_sel += 1;
                }
            }
            KeyCode::Enter => {
                if self.env_picker_sel == 0 {
                    self.env_filter.clear();
                } else {
                    self.env_filter = self
                        .envs
                        .get(self.env_picker_sel - 1)
                        .cloned()
                        .unwrap_or_default();
                }
                self.recompute_rows();
                self.mode = Mode::List;
            }
            _ => {}
        }
        None
    }

    fn handle_project_picker(&mut self, k: KeyEvent) -> Option<Action> {
        let options_count = self.projects.len() + 1;
        match k.code {
            KeyCode::Esc => self.mode = Mode::List,
            KeyCode::Up | KeyCode::Char('k') => {
                if self.project_picker_sel > 0 {
                    self.project_picker_sel -= 1;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.project_picker_sel + 1 < options_count {
                    self.project_picker_sel += 1;
                }
            }
            KeyCode::Enter => {
                if self.project_picker_sel == 0 {
                    self.project_filter.clear();
                } else {
                    self.project_filter = self
                        .projects
                        .get(self.project_picker_sel - 1)
                        .cloned()
                        .unwrap_or_default();
                }
                self.recompute_rows();
                self.mode = Mode::List;
            }
            _ => {}
        }
        None
    }

    /// Returns true and shows a toast if the current session lacks full
    /// (master-password) permission. Use to gate write/reveal operations.
    fn deny_basic(&mut self) -> bool {
        if !self.vault.permission().is_full() {
            self.toast = "Basic mode: this operation requires the master password.".into();
            self.toast_at = Instant::now();
            return true;
        }
        false
    }

    /// Open the settings popup.
    fn open_settings(&mut self) {
        self.settings_tab = SettingsTab::Password;
        self.settings_new_pw.clear();
        self.settings_confirm_pw.clear();
        self.settings_pw_field = 0;
        self.settings_pw_cursor = 0;
        self.settings_token_label.clear();
        self.settings_token_label_cursor = 0;
        self.settings_token_creating = false;
        self.settings_token_sel = 0;
        self.settings_tokens = self.vault.list_tokens().unwrap_or_default();
        self.prev_mode = self.mode;
        self.mode = Mode::Settings;
    }

    fn handle_settings(&mut self, k: KeyEvent) -> Option<Action> {
        // Tab cycles between settings tabs.
        if k.code == KeyCode::Tab {
            self.settings_tab = match self.settings_tab {
                SettingsTab::Password => SettingsTab::Tokens,
                SettingsTab::Tokens => SettingsTab::Info,
                SettingsTab::Info => SettingsTab::Password,
            };
            return None;
        }
        // Esc closes settings.
        if k.code == KeyCode::Esc {
            self.mode = self.prev_mode;
            return None;
        }

        match self.settings_tab {
            SettingsTab::Password => self.handle_settings_password(k),
            SettingsTab::Tokens => self.handle_settings_tokens(k),
            SettingsTab::Info => {
                // Info tab is read-only; just consume keys.
                None
            }
        }
    }

    fn handle_settings_password(&mut self, k: KeyEvent) -> Option<Action> {
        match k.code {
            KeyCode::Up | KeyCode::BackTab => {
                self.settings_pw_field = (self.settings_pw_field + 1) % 2;
                let text = if self.settings_pw_field == 0 {
                    &self.settings_new_pw
                } else {
                    &self.settings_confirm_pw
                };
                self.settings_pw_cursor = text.chars().count();
            }
            KeyCode::Down => {
                self.settings_pw_field = (self.settings_pw_field + 1) % 2;
                let text = if self.settings_pw_field == 0 {
                    &self.settings_new_pw
                } else {
                    &self.settings_confirm_pw
                };
                self.settings_pw_cursor = text.chars().count();
            }
            KeyCode::Left => {
                if self.settings_pw_cursor > 0 {
                    self.settings_pw_cursor -= 1;
                }
            }
            KeyCode::Right => {
                let text = self.settings_pw_text();
                let len = text.chars().count();
                if self.settings_pw_cursor < len {
                    self.settings_pw_cursor += 1;
                }
            }
            KeyCode::Backspace => {
                let cursor = self.settings_pw_cursor;
                let field = self.settings_pw_field;
                let text = if field == 0 { &mut self.settings_new_pw } else { &mut self.settings_confirm_pw };
                if cursor > 0 {
                    let chars: Vec<char> = text.chars().collect();
                    let new: String = chars[..cursor - 1]
                        .iter()
                        .chain(chars[cursor..].iter())
                        .collect();
                    *text = new;
                    self.settings_pw_cursor = cursor - 1;
                }
            }
            KeyCode::Char(c) => {
                let cursor = self.settings_pw_cursor;
                let field = self.settings_pw_field;
                let text = if field == 0 { &mut self.settings_new_pw } else { &mut self.settings_confirm_pw };
                let chars: Vec<char> = text.chars().collect();
                let new: String = chars[..cursor]
                    .iter()
                    .chain(std::iter::once(&c))
                    .chain(chars[cursor..].iter())
                    .collect();
                *text = new;
                self.settings_pw_cursor = cursor + 1;
            }
            KeyCode::Enter => {
                // In the "new password" field, Enter moves to "confirm password".
                if self.settings_pw_field == 0 {
                    self.settings_pw_field = 1;
                    self.settings_pw_cursor = self.settings_confirm_pw.chars().count();
                    return None;
                }
                // In the "confirm password" field, Enter submits.
                if self.settings_new_pw.is_empty() {
                    self.toast_info("New password cannot be empty.");
                    return None;
                }
                if self.settings_new_pw != self.settings_confirm_pw {
                    self.toast_info("Passwords do not match.");
                    return None;
                }
                match self.vault.change_password(&self.settings_new_pw) {
                    Ok(()) => {
                        self.toast_ok_msg_for(
                            "Master password changed. All tokens revoked.",
                            Duration::from_millis(1500),
                        );
                        self.settings_new_pw.clear();
                        self.settings_confirm_pw.clear();
                        self.settings_pw_cursor = 0;
                        self.settings_pw_field = 0;
                        self.mode = self.prev_mode;
                    }
                    Err(e) => {
                        self.toast_info(format!("Failed: {e}"));
                    }
                }
            }
            _ => {}
        }
        None
    }

    fn settings_pw_text(&self) -> &str {
        if self.settings_pw_field == 0 {
            &self.settings_new_pw
        } else {
            &self.settings_confirm_pw
        }
    }

    fn handle_settings_tokens(&mut self, k: KeyEvent) -> Option<Action> {
        if self.settings_token_creating {
            // Label input is focused.
            match k.code {
                KeyCode::Esc => {
                    self.settings_token_creating = false;
                    self.settings_token_label.clear();
                    self.settings_token_label_cursor = 0;
                }
                KeyCode::Enter => {
                    let label = if self.settings_token_label.is_empty() {
                        format!("token-{}", self.settings_tokens.len() + 1)
                    } else {
                        self.settings_token_label.clone()
                    };
                    match self.vault.create_token(&label) {
                        Ok(token) => {
                            match crate::clipboard::copy_and_clear_after(&token, 60) {
                                Ok(_) => {
                                    self.toast = format!("Token `{}` copied to clipboard (60s).", label);
                                    self.toast_ok = true;
                                }
                                Err(_) => {
                                    self.toast = format!("Token created but clipboard failed: {}", token);
                                    self.toast_ok = false;
                                }
                            }
                            self.toast_at = Instant::now();
                            self.settings_tokens = self.vault.list_tokens().unwrap_or_default();
                            self.settings_token_creating = false;
                            self.settings_token_label.clear();
                            self.settings_token_label_cursor = 0;
                        }
                        Err(e) => {
                            self.toast = format!("Failed: {e}");
                            self.toast_at = Instant::now();
                            self.toast_ok = false;
                        }
                    }
                }
                KeyCode::Left => {
                    if self.settings_token_label_cursor > 0 {
                        self.settings_token_label_cursor -= 1;
                    }
                }
                KeyCode::Right => {
                    let len = self.settings_token_label.chars().count();
                    if self.settings_token_label_cursor < len {
                        self.settings_token_label_cursor += 1;
                    }
                }
                KeyCode::Backspace => {
                    if self.settings_token_label_cursor > 0 {
                        let chars: Vec<char> = self.settings_token_label.chars().collect();
                        let new: String = chars[..self.settings_token_label_cursor - 1]
                            .iter()
                            .chain(chars[self.settings_token_label_cursor..].iter())
                            .collect();
                        self.settings_token_label = new;
                        self.settings_token_label_cursor -= 1;
                    }
                }
                KeyCode::Char(c) => {
                    let chars: Vec<char> = self.settings_token_label.chars().collect();
                    let new: String = chars[..self.settings_token_label_cursor]
                        .iter()
                        .chain(std::iter::once(&c))
                        .chain(chars[self.settings_token_label_cursor..].iter())
                        .collect();
                    self.settings_token_label = new;
                    self.settings_token_label_cursor += 1;
                }
                _ => {}
            }
            return None;
        }

        // Token list navigation.
        match k.code {
            KeyCode::Up => {
                if self.settings_token_sel > 0 {
                    self.settings_token_sel -= 1;
                }
            }
            KeyCode::Down => {
                if self.settings_token_sel + 1 < self.settings_tokens.len() {
                    self.settings_token_sel += 1;
                }
            }
            KeyCode::Char('c') | KeyCode::Char('n') => {
                if self.vault.permission().is_full() {
                    self.settings_token_creating = true;
                    self.settings_token_label.clear();
                    self.settings_token_label_cursor = 0;
                } else {
                    self.toast = "Basic mode: cannot create tokens.".into();
                    self.toast_at = Instant::now();
                    self.toast_ok = false;
                }
            }
            KeyCode::Char('x') | KeyCode::Delete => {
                if let Some(t) = self.settings_tokens.get(self.settings_token_sel) {
                    let query = t.id.to_string();
                    match self.vault.revoke_token(&query) {
                        Ok(true) => {
                            self.toast = format!("Token `{}` revoked.", t.label);
                            self.toast_ok = true;
                            self.toast_at = Instant::now();
                            self.settings_tokens = self.vault.list_tokens().unwrap_or_default();
                            if self.settings_token_sel >= self.settings_tokens.len() {
                                self.settings_token_sel = self.settings_tokens.len().saturating_sub(1);
                            }
                        }
                        Ok(false) => {
                            self.toast = "Token not found.".into();
                            self.toast_at = Instant::now();
                            self.toast_ok = false;
                        }
                        Err(e) => {
                            self.toast = format!("Failed: {e}");
                            self.toast_at = Instant::now();
                            self.toast_ok = false;
                        }
                    }
                }
            }
            _ => {}
        }
        None
    }

    fn start_add(&mut self) {
        self.form = Form::new();
        self.mode = Mode::Add;
    }

    fn start_edit(&mut self) {
        if self.selected_record().is_some() {
            self.reveal_password.clear();
            self.reveal_cursor = 0;
            self.pending_edit = true;
            self.prev_mode = self.mode;
            self.mode = Mode::RevealPrompt;
        }
    }

    /// Actual edit setup — called after the master password is verified.
    fn do_edit(&mut self) {
        if let Some(rec) = self.selected_record().cloned() {
            if let Ok(dec) = self.vault.decrypt(&rec) {
                let detected = credential::detect(&dec.secret);
                // Preserve a previously-manual kind: if the stored kind differs
                // from what auto-detection would produce, treat it as manual.
                let stored_kind = rec.kind_enum();
                let kind_manual = detected.kind != stored_kind;
                let custom_fields = dec
                    .custom_fields
                    .iter()
                    .map(|f| CustomFieldForm {
                        key: f.key.clone(),
                        value: f.value.clone(),
                        masked: f.masked,
                    })
                    .collect();
                self.form = Form {
                    name: dec.name.clone(),
                    secret: dec.secret.clone(),
                    kind: stored_kind,
                    kind_manual,
                    env: dec.environment.clone(),
                    project: dec.project.clone(),
                    env_var: dec.env_var.clone(),
                    notes: dec.notes.clone(),
                    field: FormField::Name,
                    detection: Some(detected),
                    editing_id: Some(dec.id),
                    custom_fields,
                    cursor: 0,
                };
                self.form.sync_cursor_to_end();
                self.mode = Mode::Edit;
            }
        }
    }

    fn handle_form(&mut self, k: KeyEvent) -> Option<Action> {
        // The Kind field doubles as a selector and a custom-text input:
        // arrows cycle through known kinds; typing a character starts (or
        // appends to) a user-defined kind name; Backspace edits/resets.
        if self.form.field == FormField::Kind {
            match k.code {
                KeyCode::Esc => self.mode = Mode::List,
                KeyCode::Tab | KeyCode::Down => {
                    self.form.field = self.form.field.next(&self.form);
                    self.form.sync_cursor_to_end();
                }
                KeyCode::BackTab | KeyCode::Up => {
                    self.form.field = self.form.field.prev(&self.form);
                    self.form.sync_cursor_to_end();
                }
                KeyCode::Enter => {
                    self.form.field = self.form.field.next(&self.form);
                    self.form.sync_cursor_to_end();
                }
                KeyCode::Left => self.cycle_kind(-1),
                KeyCode::Right => self.cycle_kind(1),
                KeyCode::Backspace => self.backspace_kind(),
                KeyCode::Char(c) if c.is_alphanumeric() || c == '-' || c == '_' => {
                    self.form.kind_manual = true;
                    match &mut self.form.kind {
                        CredentialKind::Custom(s) => s.push(c),
                        _ => self.form.kind = CredentialKind::Custom(c.to_string()),
                    }
                    if self.form.env_var.is_empty() {
                        self.form.env_var = self.form.kind.env_var().to_string();
                    }
                }
                _ => {}
            }
            return None;
        }

        // The Masked toggle: Space / Left / Right flips the flag.
        // Backspace on this toggle removes the whole custom field row.
        if let FormField::CustomMasked(i) = self.form.field {
            match k.code {
                KeyCode::Esc => self.mode = Mode::List,
                KeyCode::Tab | KeyCode::Down => {
                    self.form.field = self.form.field.next(&self.form);
                    self.form.sync_cursor_to_end();
                }
                KeyCode::BackTab | KeyCode::Up => {
                    self.form.field = self.form.field.prev(&self.form);
                    self.form.sync_cursor_to_end();
                }
                KeyCode::Enter => {
                    self.form.field = self.form.field.next(&self.form);
                    self.form.sync_cursor_to_end();
                }
                KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right => {
                    if let Some(f) = self.form.custom_fields.get_mut(i) {
                        f.masked = !f.masked;
                    }
                }
                KeyCode::Backspace => { self.remove_custom_field(i); }
                _ => {}
            }
            return None;
        }

        // The AddField button: Enter appends a new custom field.
        if self.form.field == FormField::AddField {
            match k.code {
                KeyCode::Esc => self.mode = Mode::List,
                KeyCode::Tab | KeyCode::Down => {
                    self.form.field = self.form.field.next(&self.form);
                    self.form.sync_cursor_to_end();
                }
                KeyCode::BackTab | KeyCode::Up => {
                    self.form.field = self.form.field.prev(&self.form);
                    self.form.sync_cursor_to_end();
                }
                KeyCode::Enter => {
                    self.form.custom_fields.push(CustomFieldForm::default());
                    let idx = self.form.custom_fields.len() - 1;
                    self.form.field = FormField::CustomKey(idx);
                    self.form.sync_cursor_to_end();
                }
                _ => {}
            }
            return None;
        }

        // Custom field key/value: text input.
        if matches!(self.form.field, FormField::CustomKey(_) | FormField::CustomValue(_)) {
            match k.code {
                KeyCode::Esc => self.mode = Mode::List,
                KeyCode::Tab | KeyCode::Down => {
                    self.form.field = self.form.field.next(&self.form);
                    self.form.sync_cursor_to_end();
                }
                KeyCode::BackTab | KeyCode::Up => {
                    self.form.field = self.form.field.prev(&self.form);
                    self.form.sync_cursor_to_end();
                }
                KeyCode::Enter => {
                    self.form.field = self.form.field.next(&self.form);
                    self.form.sync_cursor_to_end();
                }
                KeyCode::Left => self.form.cursor_left(),
                KeyCode::Right => self.form.cursor_right(),
                KeyCode::Backspace => {
                    self.form.delete_before_cursor();
                }
                KeyCode::Char(c) => {
                    self.form.insert_char(c);
                }
                _ => {}
            }
            return None;
        }

        // Standard text fields.
        match k.code {
            KeyCode::Esc => self.mode = Mode::List,
            KeyCode::Tab | KeyCode::Down => {
                self.form.field = self.form.field.next(&self.form);
                self.form.sync_cursor_to_end();
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.form.field = self.form.field.prev(&self.form);
                self.form.sync_cursor_to_end();
            }
            KeyCode::Enter if self.form.field == FormField::Notes => {
                return self.save_form();
            }
            KeyCode::Enter => {
                self.form.field = self.form.field.next(&self.form);
                self.form.sync_cursor_to_end();
            }
            KeyCode::Left => self.form.cursor_left(),
            KeyCode::Right => self.form.cursor_right(),
            KeyCode::Backspace => {
                self.form.delete_before_cursor();
                if self.form.field == FormField::Secret {
                    self.form.recompute_detection();
                }
            }
            KeyCode::Char(c) => {
                self.form.insert_char(c);
                if self.form.field == FormField::Secret {
                    self.form.recompute_detection();
                }
            }
            _ => {}
        }
        None
    }

    /// Remove a custom field by index and fix up the focus.
    fn remove_custom_field(&mut self, idx: usize) {
        if idx < self.form.custom_fields.len() {
            self.form.custom_fields.remove(idx);
            // Adjust focus: stay on the same index (or step back / go to AddField).
            let len = self.form.custom_fields.len();
            self.form.field = if len == 0 {
                FormField::AddField
            } else if idx >= len {
                FormField::CustomKey(len - 1)
            } else {
                FormField::CustomKey(idx)
            };
        }
    }

    /// Cycle the manually-selected credential kind by `delta` positions.
    /// Custom kinds aren't in the known list, so cycling from a Custom kind
    /// jumps to the first (delta > 0) or last (delta < 0) known kind.
    fn cycle_kind(&mut self, delta: i32) {
        let all = CredentialKind::all();
        let len = all.len() as i32;
        let idx = match &self.form.kind {
            CredentialKind::Custom(_) => {
                if delta > 0 {
                    0
                } else {
                    len - 1
                }
            }
            k => all.iter().position(|x| x == k).unwrap_or(0) as i32,
        };
        let new_idx = ((idx + delta) % len + len) % len;
        let old_kind = self.form.kind.clone();
        self.form.kind = all[new_idx as usize].clone();
        self.form.kind_manual = true;
        // Follow the new kind's env_var suggestion if env_var is empty or was the
        // auto-suggestion for the previous kind.
        if self.form.env_var.is_empty() || self.form.env_var.as_str() == old_kind.env_var() {
            self.form.env_var = self.form.kind.env_var().to_string();
        }
    }

    /// Backspace on the Kind field: edit a custom name, or reset to auto.
    fn backspace_kind(&mut self) {
        match &mut self.form.kind {
            CredentialKind::Custom(s) => {
                s.pop();
                if s.is_empty() {
                    self.form.kind_manual = false;
                    self.form.recompute_detection();
                }
            }
            _ => {
                self.form.kind_manual = false;
                self.form.recompute_detection();
            }
        }
    }

    fn save_form(&mut self) -> Option<Action> {
        let f = &self.form;
        if f.name.trim().is_empty() {
            self.toast_info("Name is required.");
            return None;
        }
        if f.secret.trim().is_empty() {
            self.toast_info("Secret is required.");
            return None;
        }
        let kind = self.form.kind.clone();
        let env_var = if f.env_var.trim().is_empty() {
            kind.env_var().to_string()
        } else {
            f.env_var.trim().to_string()
        };
        // Collect custom fields as (key, value, masked) tuples.
        let custom: Vec<(String, String, bool)> = self
            .form
            .custom_fields
            .iter()
            .map(|cf| (cf.key.clone(), cf.value.clone(), cf.masked))
            .collect();

        let result = if let Some(id) = f.editing_id {
            self.vault
                .update(
                    id,
                    Some(f.name.trim()),
                    Some(f.env.trim()),
                    Some(f.project.trim()),
                    Some(f.secret.trim()),
                    Some(&env_var),
                    Some(f.notes.trim()),
                    Some(kind),
                )
                .and_then(|_| self.vault.set_custom_fields(id, &custom))
                .and_then(|_| self.vault.get(id))
        } else {
            self.vault
                .add(
                    f.name.trim(),
                    kind,
                    f.env.trim(),
                    f.project.trim(),
                    f.secret.trim(),
                    &env_var,
                    f.notes.trim(),
                )
                .and_then(|id| self.vault.set_custom_fields(id, &custom).map(|_| id))
                .map(|_| None::<CredentialRecord>)
        };

        match result {
            Ok(_) => {
                self.toast_ok_msg(format!("Saved `{}`.", f.name));
                self.reload();
                self.mode = Mode::List;
            }
            Err(e) => {
                self.toast_info(format!("Save failed: {e}"));
            }
        }
        None
    }

    fn copy_selected(&mut self) -> Option<Action> {
        if let Some(rec) = self.selected_record().cloned() {
            if let Ok(dec) = self.vault.decrypt(&rec) {
                let secret = dec.secret.clone();
                let name = dec.name.clone();
                // Trigger green flash on the selected list row.
                self.copied_in_list = true;
                self.copied_at = Instant::now();
                self.toast_info(format!("Copying `{}`…", name));
                return Some(Action::CopyAndNotify {
                    name,
                    secret,
                    secs: clipboard::DEFAULT_CLEAR_SECS,
                });
            } else {
                self.toast_info("Decrypt failed.");
            }
        }
        None
    }

    fn handle_detail(&mut self, k: KeyEvent) -> Option<Action> {
        // Number of copyable fields: 7 fixed (name, kind, env, project, env_var,
        // secret, notes) + custom fields count.
        const FIXED: usize = 7;
        let field_count = self
            .detail
            .as_ref()
            .map(|d| FIXED + d.custom_fields.len())
            .unwrap_or(0);
        // Default copy target is Secret (index 5) when nothing is selected.
        const SECRET_IDX: usize = 5;

        // Shift+number/letter: quick-copy a field by its marker index.
        if k.modifiers.contains(KeyModifiers::SHIFT) {
            if let Some(idx) = shift_field_index(&k) {
                if idx < field_count {
                    self.detail_field_sel = Some(idx);
                    return self.copy_detail_field(idx);
                }
            }
        }

        match k.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('i') | KeyCode::Char(' ') => {
                self.mode = Mode::List;
            }
            KeyCode::PageDown => {
                self.detail_scroll = self.detail_scroll.saturating_add(1);
            }
            KeyCode::PageUp => {
                self.detail_scroll = self.detail_scroll.saturating_sub(1);
            }
            KeyCode::Tab | KeyCode::Down => {
                if field_count > 0 {
                    self.detail_field_sel = Some(match self.detail_field_sel {
                        None => 0,
                        Some(i) if i + 1 < field_count => i + 1,
                        _ => 0,
                    });
                }
            }
            KeyCode::Up => {
                if field_count > 0 {
                    self.detail_field_sel = Some(match self.detail_field_sel {
                        None => field_count - 1,
                        Some(0) => field_count - 1,
                        Some(i) => i - 1,
                    });
                }
            }
            KeyCode::Char('c') | KeyCode::Enter => {
                // Copy selected field, or secret if nothing selected.
                let idx = self.detail_field_sel.unwrap_or(SECRET_IDX);
                return self.copy_detail_field(idx);
            }
            KeyCode::Char('s') => {
                if self.show_secret_in_detail {
                    self.show_secret_in_detail = false;
                } else if !self.deny_basic() {
                    self.reveal_password.clear();
                    self.reveal_cursor = 0;
                    self.prev_mode = Mode::Detail;
                    self.mode = Mode::RevealPrompt;
                }
            }
            KeyCode::Char('e') => {
                if !self.deny_basic() {
                    self.start_edit();
                }
            }
            _ => {}
        }
        None
    }

    /// Copy a field from the detail view by index.
    /// 0=name, 1=kind, 2=env, 3=project, 4=env_var, 5=secret, 6=notes, 7+=custom
    fn copy_detail_field(&mut self, idx: usize) -> Option<Action> {
        let d = self.detail.as_ref()?;
        let (label, value): (&str, String) = match idx {
            0 => ("name", d.name.clone()),
            1 => ("kind", d.kind.label().to_string()),
            2 => ("env", d.environment.clone()),
            3 => ("project", d.project.clone()),
            4 => ("env_var", d.env_var.clone()),
            5 => ("secret", d.secret.clone()),
            6 => ("notes", d.notes.clone()),
            i => {
                let cf = d.custom_fields.get(i - 7)?;
                (cf.key.as_str(), cf.value.clone())
            }
        };
        let name = d.name.clone();
        // Trigger green flash on the copied field's marker.
        self.copied_field_idx = Some(idx);
        self.copied_at = Instant::now();
        self.toast_info(format!("Copying `{}` ({})…", name, label));
        Some(Action::CopyAndNotify {
            name,
            secret: value,
            secs: clipboard::DEFAULT_CLEAR_SECS,
        })
    }

    /// Password confirmation prompt for revealing a secret or editing a credential.
    fn handle_reveal_prompt(&mut self, k: KeyEvent) -> Option<Action> {
        match k.code {
            KeyCode::Esc => {
                self.reveal_password.clear();
                self.reveal_cursor = 0;
                self.pending_edit = false;
                self.mode = self.prev_mode;
            }
            KeyCode::Enter => {
                let pw = std::mem::take(&mut self.reveal_password);
                self.reveal_cursor = 0;
                if self.vault.verify_password(&pw) {
                    if self.pending_edit {
                        self.pending_edit = false;
                        self.do_edit();
                    } else {
                        self.show_secret_in_detail = true;
                        self.toast = "Secret revealed.".into();
                        self.toast_at = Instant::now();
                        self.mode = self.prev_mode;
                    }
                } else {
                    self.toast = "Wrong password.".into();
                    self.toast_at = Instant::now();
                    self.reveal_password.clear();
                    self.pending_edit = false;
                    self.mode = self.prev_mode;
                }
            }
            KeyCode::Left => {
                self.reveal_cursor = self.reveal_cursor.saturating_sub(1);
            }
            KeyCode::Right => {
                let max = self.reveal_password.chars().count();
                if self.reveal_cursor < max {
                    self.reveal_cursor += 1;
                }
            }
            KeyCode::Backspace => {
                text_delete_before(&mut self.reveal_password, &mut self.reveal_cursor);
            }
            KeyCode::Char(c) => {
                text_insert(&mut self.reveal_password, &mut self.reveal_cursor, c);
            }
            _ => {}
        }
        None
    }

    fn handle_confirm_delete(&mut self, k: KeyEvent) -> Option<Action> {
        match k.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                if let Some(rec) = self.selected_record().cloned() {
                    match self.vault.delete(rec.id) {
                        Ok(true) => {
                            self.toast = format!("Deleted `{}`.", rec.name);
                            self.toast_at = Instant::now();
                            self.reload();
                        }
                        Ok(false) => {
                            self.toast = "Already gone.".into();
                            self.toast_at = Instant::now();
                        }
                        Err(e) => {
                            self.toast = format!("Delete failed: {e}");
                            self.toast_at = Instant::now();
                        }
                    }
                }
                self.mode = Mode::List;
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('q') => self.mode = Mode::List,
            _ => {}
        }
        None
    }

    fn handle_help(&mut self, k: KeyEvent) -> Option<Action> {
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter | KeyCode::Char('?') => {
                self.mode = self.prev_mode;
            }
            _ => {}
        }
        None
    }
}
