//! Rendering for the DevCred TUI.

use crate::credential::CredentialKind;
use crate::tui::{App, Focus, FormField, Mode};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, List, ListItem, Paragraph, Row, Table, TableState, Wrap,
};
use ratatui::Frame;
use std::time::{Duration, Instant};

/// Accent color used for highlights and the selected row.
const ACCENT: Color = Color::Cyan;
const DIM: Color = Color::DarkGray;
const WARN: Color = Color::Yellow;
const OK: Color = Color::Green;
const NORMAL: Color = Color::White;

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(5), Constraint::Length(3)])
        .split(area);

    draw_header(f, app, chunks[0]);
    draw_body(f, app, chunks[1]);
    draw_footer(f, app, chunks[2]);

    // Overlays drawn last so they sit on top.
    match app.mode {
        Mode::Search => draw_search_overlay(f, app),
        Mode::FilterEnv => draw_env_picker(f, app),
        Mode::FilterProject => draw_project_picker(f, app),
        Mode::Add | Mode::Edit => draw_form(f, app),
        Mode::Detail => draw_detail(f, app),
        Mode::ConfirmDelete => draw_confirm_delete(f, app),
        Mode::RevealPrompt => draw_reveal_prompt(f, app),
        Mode::Help => draw_help(f, app),
        Mode::List => {}
    }
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let count = app.rows.len();
    let total = app.all.len();
    let filter_bits: Vec<String> = [
        (!app.env_filter.is_empty()).then(|| format!("env={}", app.env_filter)),
        (!app.project_filter.is_empty()).then(|| format!("project={}", app.project_filter)),
        (!app.search.is_empty()).then(|| format!("/{}", app.search)),
    ]
    .into_iter()
    .flatten()
    .collect();
    let filter_str = if filter_bits.is_empty() {
        "no filter".to_string()
    } else {
        filter_bits.join(" ")
    };

    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled("DevCred", Style::default().bg(ACCENT).fg(Color::Black).add_modifier(Modifier::BOLD)),
        Span::raw(" "),
        Span::styled(filter_str, Style::default().fg(DIM)),
        Span::raw(" "),
        Span::styled(
            format!("{count}/{total} credentials"),
            Style::default().fg(Color::White),
        ),
        Span::raw(" ")
    ]);

    let block = Block::default().borders(Borders::ALL).title(title);
    let para = Paragraph::new(Text::from("")).block(block);
    f.render_widget(para, area);
}

fn draw_body(f: &mut Frame, app: &mut App, area: Rect) {
    // Split into left (category sidebar) + right (credential list).
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(24), Constraint::Min(40)])
        .split(area);
    draw_category_pane(f, app, body[0]);
    draw_credential_list(f, app, body[1]);
}

fn draw_category_pane(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Category && app.mode == Mode::List;
    let border_color = if focused { ACCENT } else { DIM };

    // Build the list: "All (N)" + one entry per kind with count.
    let total = app.all.len();
    let mut items: Vec<ListItem> = Vec::new();
    // Index 0 = All
    let all_style = if app.category_sel == 0 {
        Style::default().bg(ACCENT).fg(Color::Black).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };
    items.push(ListItem::new(Line::from(vec![
        Span::styled(if app.category_sel == 0 { "▸ " } else { "  " }, all_style),
        Span::styled(format!("All"), all_style),
        Span::styled(format!("  ({})", total), Style::default().fg(DIM)),
    ])));

    for (i, (kind, count)) in app.categories.iter().enumerate() {
        let is_sel = app.category_sel == i + 1;
        let style = if is_sel {
            Style::default().bg(ACCENT).fg(Color::Black).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        items.push(ListItem::new(Line::from(vec![
            Span::styled(if is_sel { "▸ " } else { "  " }, style),
            Span::styled(kind.label(), style),
            Span::styled(format!("  ({})", count), Style::default().fg(DIM)),
        ])));
    }

    let title = if focused {
        " Categories ◄ "
    } else {
        " Categories "
    };
    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .title(Span::styled(title, Style::default().fg(border_color))),
    );
    f.render_widget(list, area);
}

fn draw_credential_list(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::List && app.mode == Mode::List;
    let border_color = if focused { ACCENT } else { DIM };

    if app.rows.is_empty() {
        let msg = if app.all.is_empty() {
            "No credentials yet. Press `n` to add one."
        } else {
            "No matches in this category.\nPress Tab to switch to categories."
        };
        let para = Paragraph::new(msg)
            .style(Style::default().fg(DIM))
            .alignment(Alignment::Center)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(border_color))
                    .title(Span::styled(
                        if focused { " Credentials ► " } else { " Credentials " },
                        Style::default().fg(border_color),
                    )),
            );
        f.render_widget(para, area);
        return;
    }

    let header_cells = ["ID", "NAME", "KIND", "ENV", "PROJECT", "ENV_VAR"]
        .iter()
        .map(|h| Cell::from(*h).style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)));
    let header = Row::new(header_cells).height(1).bottom_margin(0);

    let rows = app.rows.iter().enumerate().map(|(i, r)| {
        let selected = i == app.selected;
        // Green flash on the selected row for 1.5s after a list copy.
        let list_flash = app.copied_in_list
            && selected
            && Instant::now().duration_since(app.copied_at) < Duration::from_millis(1500);
        let style = if list_flash {
            Style::default().bg(Color::Green).fg(Color::Black).add_modifier(Modifier::BOLD)
        } else if selected && focused {
            Style::default().bg(ACCENT).fg(Color::Black).add_modifier(Modifier::BOLD)
        } else if selected {
            Style::default().bg(Color::DarkGray).fg(Color::White)
        } else {
            Style::default()
        };
        let cells = [
            Cell::from(r.rec.id.to_string()),
            Cell::from(r.rec.name.as_str()),
            Cell::from(r.rec.kind.as_str()),
            Cell::from(r.rec.environment.as_str()),
            Cell::from(r.rec.project.as_str()),
            Cell::from(r.rec.env_var.as_str()),
        ];
        Row::new(cells).style(style)
    });

    let widths = [
        Constraint::Length(5),
        Constraint::Min(20),
        Constraint::Length(12),
        Constraint::Length(12),
        Constraint::Length(16),
        Constraint::Min(16),
    ];
    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color))
                .title(Span::styled(
                    if focused { " Credentials ► " } else { " Credentials " },
                    Style::default().fg(border_color),
                )),
        );
    // Use a stateful render so the table auto-scrolls to keep the selected
    // row visible when the list is longer than the viewport.
    // Per-row styles (set above) handle the highlight colors; we don't set
    // row_highlight_style because it would override the per-row background.
    let mut state = TableState::default();
    state.select(Some(app.selected));
    f.render_stateful_widget(table, area, &mut state);
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let hint = Line::from(vec![
        // Common keys (left side)
        Span::styled("Tab", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
        Span::raw(":switch  "),
        Span::styled("↑/↓", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
        Span::raw(":navigate  "),
        Span::styled("/", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
        Span::raw(":search  "),
        Span::styled("e", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
        Span::raw(":env  "),
        Span::styled("p", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
        Span::raw(":project  "),
        Span::styled("?", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
        Span::raw(":help  "),
        Span::styled("Esc/q", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
        Span::raw(":quit  "),
        // Separator
        Span::styled("|", Style::default().fg(DIM)),
        Span::raw("  "),
        // Credential-specific keys (right side)
        Span::styled("n", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
        Span::raw(":new  "),
        Span::styled("c/↲", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
        Span::raw(":copy  "),
        Span::styled("i", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
        Span::raw(":detail  "),
        Span::styled("r", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
        Span::raw(":edit  "),
        Span::styled("d", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
        Span::raw(":delete"),
    ]);

    let toast = if !app.toast.is_empty() {
        let bg = if app.toast_ok { Color::Green } else { Color::Blue };
        vec![Span::styled(
            format!(" {} ", app.toast),
            Style::default().bg(bg).fg(Color::White),
        )]
    } else {
        vec![]
    };

    let block = Block::default().borders(Borders::ALL);
    let para = Paragraph::new(vec![hint, Line::from(toast)]).block(block);
    f.render_widget(para, area);
}

fn centered_rect(area: Rect, w: u16, h: u16) -> Rect {
    let popup = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage((100 - h) / 2), Constraint::Percentage(h), Constraint::Percentage((100 - h) / 2)])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage((100 - w) / 2), Constraint::Percentage(w), Constraint::Percentage((100 - w) / 2)])
        .split(popup[1])[1]
}

fn draw_search_overlay(f: &mut Frame, app: &App) {
    let area = f.area();
    let bar = Rect { x: area.x + 1, y: area.y + 1, width: area.width.saturating_sub(2), height: 1 };
    let prompt = format!("/{}", app.search);
    let para = Paragraph::new(prompt.as_str()).style(Style::default().fg(ACCENT));
    f.render_widget(para, bar);
    // Cursor
    let cx = bar.x + prompt.len() as u16;
    f.set_cursor_position((cx, bar.y));
}

fn draw_env_picker(f: &mut Frame, app: &App) {
    let area = f.area();
    let rect = centered_rect(area, 40, 50);
    f.render_widget(Clear, rect);

    let mut items: Vec<ListItem> = vec![ListItem::new("✦ (all environments)").style(Style::default().fg(ACCENT))];
    for e in &app.envs {
        items.push(ListItem::new(format!("  {e}")));
    }
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Filter by environment"))
        .highlight_style(Style::default().bg(ACCENT).fg(Color::Black).add_modifier(Modifier::BOLD));
    f.render_stateful_widget(list, rect, &mut ratatui::widgets::ListState::default().with_selected(Some(app.env_picker_sel)));
}

fn draw_project_picker(f: &mut Frame, app: &App) {
    let area = f.area();
    let rect = centered_rect(area, 40, 50);
    f.render_widget(Clear, rect);

    let mut items: Vec<ListItem> = vec![ListItem::new("✦ (all projects)").style(Style::default().fg(ACCENT))];
    for p in &app.projects {
        items.push(ListItem::new(format!("  {p}")));
    }
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Filter by project"))
        .highlight_style(Style::default().bg(ACCENT).fg(Color::Black).add_modifier(Modifier::BOLD));
    f.render_stateful_widget(list, rect, &mut ratatui::widgets::ListState::default().with_selected(Some(app.project_picker_sel)));
}

fn draw_form(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let rect = centered_rect(area, 72, 85);
    f.render_widget(Clear, rect);

    let title = if app.mode == Mode::Edit {
        "Edit credential"
    } else {
        "Add credential"
    };

    // Inner width available for content (excluding borders).
    let inner_w = rect.width.saturating_sub(2);

    let mut lines: Vec<Line> = Vec::new();
    // Track the line index of the focused field for cursor placement.
    let mut focus_line: Option<u16> = None;
    let mut focus_x: Option<u16> = None; // x offset within the line (relative to rect.x+1)

    // --- Standard fields ---
    let ordered: [FormField; 7] = [
        FormField::Name,
        FormField::Secret,
        FormField::Kind,
        FormField::Env,
        FormField::Project,
        FormField::EnvVar,
        FormField::Notes,
    ];
    for field in ordered {
        let is_current = app.form.field == field;
        let label_style = if is_current {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(NORMAL)
        };
        if field == FormField::Kind {
            let suffix = if app.form.kind_manual { " (manual)" } else { " (auto)" };
            let hint = if is_current {
                "  ←/→ cycle · type to customize"
            } else {
                ""
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{:<12}", field.label()), label_style),
                Span::raw(" "),
                Span::styled(app.form.kind.label(), Style::default().fg(Color::White)),
                Span::styled(suffix, Style::default().fg(DIM)),
                Span::styled(hint, Style::default().fg(ACCENT)),
            ]));
            if is_current {
                focus_line = Some(lines.len() as u16 - 1);
                if let CredentialKind::Custom(s) = &app.form.kind {
                    focus_x = Some(13 + s.chars().count() as u16 + " (manual)".len() as u16);
                }
            }
            continue;
        }
        let value = match field {
            FormField::Name => &app.form.name,
            FormField::Secret => &app.form.secret,
            FormField::Env => &app.form.env,
            FormField::Project => &app.form.project,
            FormField::EnvVar => &app.form.env_var,
            FormField::Notes => &app.form.notes,
            _ => unreachable!(),
        };
        let display = if field == FormField::Secret && !is_current && !value.is_empty() {
            "•".repeat(value.chars().count().min(20))
        } else {
            value.clone()
        };
        // Horizontal scroll: when focused, show the end of the text so the
        // cursor stays visible.  avail = inner_w - 12 (label) - 1 (space) - 1 (cursor)
        let avail = inner_w.saturating_sub(14);
        let (display, val_len) = if is_current {
            scroll_to_end(&display, avail)
        } else {
            let len = display.chars().count() as u16;
            (display, len)
        };
        let cursor = if is_current { "▏" } else { " " };
        lines.push(Line::from(vec![
            Span::styled(format!("{:<12}", field.label()), label_style),
            Span::raw(" "),
            Span::styled(display, Style::default().fg(Color::White)),
            Span::styled(cursor, Style::default().fg(ACCENT)),
        ]));
        if is_current {
            focus_line = Some(lines.len() as u16 - 1);
            focus_x = Some(13 + val_len);
        }
    }

    // --- Custom fields section ---
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(" Custom Fields", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
        Span::styled("  (shown in detail view only)", Style::default().fg(DIM)),
    ]));

    for (i, cf) in app.form.custom_fields.iter().enumerate() {
        let key_focused = app.form.field == FormField::CustomKey(i);
        let val_focused = app.form.field == FormField::CustomValue(i);
        let mask_focused = app.form.field == FormField::CustomMasked(i);

        let key_style = if key_focused {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        let val_style = if val_focused {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        let mask_style = if mask_focused {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(DIM)
        };

        // Mask the value display if not focused and masked is true.
        let val_display = if cf.masked && !val_focused && !cf.value.is_empty() {
            "•".repeat(cf.value.chars().count().min(16))
        } else {
            cf.value.clone()
        };
        let mask_label = if cf.masked { "✓ masked" } else { "✗ plain" };

        let key_str = if cf.key.is_empty() && !key_focused { "(empty)".to_string() } else { cf.key.clone() };
        let val_str = if val_display.is_empty() && !val_focused { "(empty)".to_string() } else { val_display };

        // Horizontal scroll for focused key/value.
        // Layout: "  " + key + ": " + value + "  " + "[mask_label]"
        let mask_width = mask_label.chars().count() as u16 + 2; // "[]" + label
        let fixed_after = 2 + 2 + mask_width; // ": " + "  " + "[mask]"
        let key_avail = inner_w.saturating_sub(2 + 2 + fixed_after).max(8);
        let (key_disp, key_len) = if key_focused {
            scroll_to_end(&key_str, key_avail)
        } else {
            (key_str.clone(), key_str.chars().count() as u16)
        };
        let key_display_len = if cf.key.is_empty() && !key_focused { 7 } else { key_len };
        let val_avail = inner_w
            .saturating_sub(2 + key_display_len + 2 + 2 + mask_width)
            .max(8);
        let (val_disp, val_len) = if val_focused {
            scroll_to_end(&val_str, val_avail)
        } else {
            (val_str.clone(), val_str.chars().count() as u16)
        };

        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(key_disp, key_style),
            Span::styled(": ", Style::default().fg(DIM)),
            Span::styled(val_disp, val_style),
            Span::raw("  "),
            Span::styled(format!("[{}]", mask_label), mask_style),
        ]));

        if key_focused {
            focus_line = Some(lines.len() as u16 - 1);
            focus_x = Some(2 + key_len);
        } else if val_focused {
            focus_line = Some(lines.len() as u16 - 1);
            focus_x = Some(2 + key_display_len + 2 + val_len);
        }
    }

    // [+ Add custom field] button
    let add_focused = app.form.field == FormField::AddField;
    let add_style = if add_focused {
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(DIM)
    };
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("[+ Add custom field]", add_style),
    ]));
    if add_focused {
        focus_line = Some(lines.len() as u16 - 1);
    }

    // --- Hint line ---
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(" i ", Style::default().fg(ACCENT)),
        Span::styled(app.form.field.description(), Style::default().fg(DIM)),
    ]));

    // Detection feedback for the secret field.
    if let Some(d) = &app.form.detection {
        let (color, mark) = if d.valid {
            (OK, "✓")
        } else {
            (WARN, "!")
        };
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled(format!(" {mark} detected: "), Style::default().fg(color).add_modifier(Modifier::BOLD)),
            Span::styled(d.kind.label(), Style::default().fg(color)),
            Span::raw("  "),
            Span::styled(&d.note, Style::default().fg(DIM)),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(" Tab ", Style::default().bg(DIM).fg(Color::Black)),
        Span::raw(" next  "),
        Span::styled(" Enter ", Style::default().bg(DIM).fg(Color::Black)),
        Span::raw(" save  "),
        Span::styled(" ⌫ ", Style::default().bg(DIM).fg(Color::Black)),
        Span::raw(" on [masked] removes field  "),
        Span::styled(" Esc ", Style::default().bg(DIM).fg(Color::Black)),
        Span::raw(" cancel"),
    ]));

    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        format!(" {title} "),
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    ));
    let para = Paragraph::new(lines).block(block);
    f.render_widget(para, rect);

    // Place cursor on the active text field.
    if let (Some(cy), Some(cx)) = (focus_line, focus_x) {
        f.set_cursor_position((rect.x + 1 + cx, rect.y + 1 + cy));
    }
}

/// Truncate text to fit `avail` columns, showing the **end** so the cursor
/// (which sits at the end during editing) stays visible.
/// Returns `(display_string, visible_char_count)`.
fn scroll_to_end(text: &str, avail: u16) -> (String, u16) {
    let len = text.chars().count() as u16;
    if len <= avail || avail == 0 {
        return (text.to_string(), len);
    }
    let skip = (len - avail) as usize;
    let result: String = text.chars().skip(skip).collect();
    (result, avail)
}

fn field_marker(idx: usize) -> String {
    if idx < 9 {
        ((b'1' + idx as u8) as char).to_string()
    } else if idx == 9 {
        "0".to_string()
    } else {
        ((b'a' + (idx - 10) as u8) as char).to_string()
    }
}

fn draw_detail(f: &mut Frame, app: &App) {
    let area = f.area();
    let rect = centered_rect(area, 70, 75);
    f.render_widget(Clear, rect);

    let d = match &app.detail {
        Some(d) => d,
        None => return,
    };
    let secret_display = if app.show_secret_in_detail {
        d.secret.clone()
    } else {
        "•".repeat(d.secret.chars().count().min(24)) + " (press `s` to reveal)"
    };

    let sel = app.detail_field_sel;
    // Check if a field was just copied (green flash for 1.5s).
    let now = Instant::now();
    let copied_idx = app.copied_field_idx.filter(|_| {
        now.duration_since(app.copied_at) < Duration::from_millis(1500)
    });
    // Returns (marker_string, marker_style) for field at idx.
    let marker = |idx: usize| -> (String, Style) {
        let m = field_marker(idx);
        let style = if copied_idx == Some(idx) {
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
        } else if sel == Some(idx) {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(DIM)
        };
        (m, style)
    };
    // Returns value style: bold if selected, normal otherwise.
    let val_style = |idx: usize, base: Style| -> Style {
        if sel == Some(idx) {
            base.add_modifier(Modifier::BOLD)
        } else {
            base
        }
    };

    let mut lines: Vec<Line> = Vec::new();

    // [1] Name
    let (m, ms) = marker(0);
    lines.push(Line::from(vec![
        Span::styled("Name        ", Style::default().fg(NORMAL)),
        Span::styled(&d.name, val_style(0, Style::default().fg(Color::White))),
        Span::styled(format!("  [{}]", m), ms),
    ]));
    // [2] Kind
    let (m, ms) = marker(1);
    lines.push(Line::from(vec![
        Span::styled("Kind        ", Style::default().fg(NORMAL)),
        Span::styled(d.kind.label(), val_style(1, Style::default().fg(ACCENT))),
        Span::styled(format!("  [{}]", m), ms),
    ]));
    // [3] Environment
    let (m, ms) = marker(2);
    let env_str = if d.environment.is_empty() { "(none)" } else { &d.environment };
    lines.push(Line::from(vec![
        Span::styled("Environment ", Style::default().fg(NORMAL)),
        Span::styled(env_str, val_style(2, Style::default())),
        Span::styled(format!("  [{}]", m), ms),
    ]));
    // [4] Project
    let (m, ms) = marker(3);
    let proj_str = if d.project.is_empty() { "(none)" } else { &d.project };
    lines.push(Line::from(vec![
        Span::styled("Project     ", Style::default().fg(NORMAL)),
        Span::styled(proj_str, val_style(3, Style::default())),
        Span::styled(format!("  [{}]", m), ms),
    ]));
    // [5] Env Var
    let (m, ms) = marker(4);
    lines.push(Line::from(vec![
        Span::styled("Env Var     ", Style::default().fg(NORMAL)),
        Span::styled(&d.env_var, val_style(4, Style::default().fg(ACCENT))),
        Span::styled(format!("  [{}]", m), ms),
    ]));

    lines.push(Line::from(""));

    // [6] Secret
    let (m, ms) = marker(5);
    let secret_base = Style::default().fg(if app.show_secret_in_detail { WARN } else { DIM });
    lines.push(Line::from(vec![
        Span::styled("Secret      ", Style::default().fg(NORMAL)),
        Span::styled(secret_display, val_style(5, secret_base)),
        Span::styled(format!("  [{}]", m), ms),
    ]));

    lines.push(Line::from(""));

    // [7] Notes
    let (m, ms) = marker(6);
    let notes_str = if d.notes.is_empty() { "(none)" } else { &d.notes };
    lines.push(Line::from(vec![
        Span::styled("Notes       ", Style::default().fg(NORMAL)),
        Span::styled(notes_str, val_style(6, Style::default())),
        Span::styled(format!("  [{}]", m), ms),
    ]));

    // [8+] Custom fields
    if !d.custom_fields.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            " Custom Fields",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )));
        for (i, cf) in d.custom_fields.iter().enumerate() {
            let field_idx = i + 7; // 0-6 are fixed fields
            let (m, ms) = marker(field_idx);
            let selected = sel == Some(field_idx);
            let val_display = if cf.masked && !app.show_secret_in_detail {
                "•".repeat(cf.value.chars().count().min(16))
            } else {
                cf.value.clone()
            };
            let mask_tag = if cf.masked { " (masked)" } else { "" };
            let prefix = if selected { "► " } else { "  " };
            lines.push(Line::from(vec![
                Span::styled(prefix, Style::default().fg(ACCENT)),
                Span::styled(format!("{:<10}", cf.key), Style::default().fg(NORMAL)),
                Span::raw(": "),
                Span::styled(val_display, val_style(field_idx, Style::default().fg(Color::White))),
                Span::styled(mask_tag, Style::default().fg(NORMAL)),
                Span::styled(format!("  [{}]", m), ms),
            ]));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("Created     ", Style::default().fg(NORMAL)),
        Span::raw(fmt_ts(d.created_at)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Updated     ", Style::default().fg(NORMAL)),
        Span::raw(fmt_ts(d.updated_at)),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(" Tab/↑↓ ", Style::default().bg(DIM).fg(Color::Black)),
        Span::raw(" select  "),
        Span::styled(" c/↲ ", Style::default().bg(DIM).fg(Color::Black)),
        Span::raw(" copy  "),
        Span::styled(" ⇧1 ", Style::default().bg(DIM).fg(Color::Black)),
        Span::raw(" quick-copy  "),
        Span::styled(" PgUp/PgDn ", Style::default().bg(DIM).fg(Color::Black)),
        Span::raw(" scroll  "),
        Span::styled(" s ", Style::default().bg(DIM).fg(Color::Black)),
        Span::raw(" reveal  "),
        Span::styled(" e ", Style::default().bg(DIM).fg(Color::Black)),
        Span::raw(" edit  "),
        Span::styled(" Esc ", Style::default().bg(DIM).fg(Color::Black)),
        Span::raw(" back"),
    ]));

    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        " Credential detail ",
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    ));
    let para = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((app.detail_scroll, 0));
    f.render_widget(para, rect);
}

fn draw_confirm_delete(f: &mut Frame, app: &App) {
    let area = f.area();
    let rect = centered_rect(area, 50, 25);
    f.render_widget(Clear, rect);

    let name = app
        .selected_record()
        .map(|r| r.name.as_str())
        .unwrap_or("(unknown)");
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Delete this credential?",
            Style::default().fg(WARN).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![Span::styled("  • ", Style::default().fg(WARN)), Span::raw(name)]),
        Line::from(""),
        Line::from("  This cannot be undone."),
        Line::from(""),
        Line::from(vec![
            Span::styled(" y ", Style::default().bg(WARN).fg(Color::Black)),
            Span::raw(" delete  "),
            Span::styled(" n/Esc ", Style::default().bg(DIM).fg(Color::Black)),
            Span::raw(" cancel"),
        ]),
    ];
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        " Confirm delete ",
        Style::default().fg(WARN).add_modifier(Modifier::BOLD),
    ));
    let para = Paragraph::new(lines).block(block);
    f.render_widget(para, rect);
}

fn draw_reveal_prompt(f: &mut Frame, app: &App) {
    let area = f.area();
    let rect = centered_rect(area, 50, 30);
    f.render_widget(Clear, rect);

    // Masked password: one bullet per character.
    let masked: String = "•".repeat(app.reveal_password.chars().count());
    let cursor = "▏";

    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Reveal secret requires master password",
            Style::default().fg(WARN).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Password: ", Style::default().fg(NORMAL)),
            Span::styled(masked, Style::default().fg(Color::White)),
            Span::styled(cursor, Style::default().fg(ACCENT)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled(" Enter ", Style::default().bg(DIM).fg(Color::Black)),
            Span::raw(" confirm  "),
            Span::styled(" Esc ", Style::default().bg(DIM).fg(Color::Black)),
            Span::raw(" cancel"),
        ]),
    ];

    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        " Confirm reveal ",
        Style::default().fg(WARN).add_modifier(Modifier::BOLD),
    ));
    let para = Paragraph::new(lines).block(block);
    f.render_widget(para, rect);

    // Place cursor at the end of the masked input.
    let cx = rect.x + 1 + "  Password: ".len() as u16
        + app.reveal_password.chars().count() as u16;
    let cy = rect.y + 4;
    f.set_cursor_position((cx, cy));
}

fn draw_help(f: &mut Frame, _app: &App) {
    let area = f.area();
    let rect = centered_rect(area, 60, 75);
    f.render_widget(Clear, rect);

    let help: &[(&str, &str)] = &[
        ("Tab", "Switch focus between category sidebar and list"),
        ("↑/↓ or k/j", "Move selection"),
        ("PgUp/PgDn", "Jump by 10"),
        ("Home/End", "First/last"),
        ("/", "Fuzzy search"),
        ("e", "Filter by environment"),
        ("p", "Filter by project"),
        ("n", "New credential"),
        ("r", "Edit (revise) selected"),
        ("i / Space", "Open detail view"),
        ("Enter / c", "Copy secret to clipboard (auto-clears in 30s)"),
        ("d", "Delete selected"),
        ("s", "Reveal secret (requires master password)"),
        ("?", "This help"),
        ("q / Esc", "Quit / back"),
        ("Ctrl+C", "Force quit"),
    ];

    let lines: Vec<Line> = help
        .iter()
        .map(|(k, v)| {
            Line::from(vec![
                Span::styled(format!("  {:<14}", k), Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
                Span::raw(*v),
            ])
        })
        .collect();

    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        " Help — press ? or Esc to close ",
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    ));
    let para = Paragraph::new(Text::from(lines)).block(block);
    f.render_widget(para, rect);
}

/// Format a Unix timestamp as `YYYY-MM-DD HH:MM UTC`.
fn fmt_ts(ts: i64) -> String {
    use chrono::DateTime;
    match DateTime::from_timestamp(ts, 0) {
        Some(dt) => dt.format("%Y-%m-%d %H:%M UTC").to_string(),
        None => ts.to_string(),
    }
}
