//! Rendering for the DevCred TUI.

use crate::credential::CredentialKind;
use crate::tui::{App, Focus, FormField, Mode};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, List, ListItem, Paragraph, Row, Table, TableState,
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
        Mode::Settings => draw_settings(f, app),
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
    let is_basic = !app.vault.permission().is_full();
    let mut hint_spans: Vec<Span> = Vec::new();
    if is_basic {
        hint_spans.push(Span::styled(
            " BASIC MODE ",
            Style::default().bg(Color::Yellow).fg(Color::Black).add_modifier(Modifier::BOLD),
        ));
        hint_spans.push(Span::raw("  "));
    }
    hint_spans.extend(vec![
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
        Span::styled("s", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
        Span::raw(":settings  "),
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
    let hint = Line::from(hint_spans);

    let block = Block::default().borders(Borders::ALL);
    // Footer content area is only 1 row tall (3 rows minus 2 border rows).
    // When a toast is active, show it instead of the hint so it's visible.
    let content = if !app.toast.is_empty() {
        let bg = if app.toast_ok { Color::Green } else { Color::Blue };
        Line::from(vec![Span::styled(
            format!(" {} ", app.toast),
            Style::default().bg(bg).fg(Color::White),
        )])
    } else {
        hint
    };
    let para = Paragraph::new(content).block(block);
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
    // "/" prompt + search text, with cursor split.
    let avail = bar.width.saturating_sub(1); // -1 for "/"
    let (visible, cursor_x) = scroll_to_cursor(&app.search, app.search_cursor, avail);
    let chars: Vec<char> = visible.chars().collect();
    let cx = cursor_x as usize;
    let style = Style::default().fg(ACCENT);
    let cursor_style = Style::default().fg(ACCENT).bg(Color::DarkGray);
    let line = if cx < chars.len() {
        let before: String = chars[..cx].iter().collect();
        let cursor_char: String = chars[cx].to_string();
        let after: String = chars[cx + 1..].iter().collect();
        Line::from(vec![
            Span::styled("/", style),
            Span::styled(before, style),
            Span::styled(cursor_char, cursor_style),
            Span::styled(after, style),
        ])
    } else {
        Line::from(vec![
            Span::styled("/", style),
            Span::styled(visible.clone(), style),
            Span::styled(" ", cursor_style),
        ])
    };
    let para = Paragraph::new(line);
    f.render_widget(para, bar);
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
        // Horizontal scroll: when focused, show a window around the cursor.
        // avail = inner_w - 12 (label) - 1 (space)
        let avail = inner_w.saturating_sub(13);
        if is_current {
            let (visible, cursor_x) = scroll_to_cursor(&display, app.form.cursor, avail);
            let chars: Vec<char> = visible.chars().collect();
            let cx = cursor_x as usize;
            let val_style = Style::default().fg(Color::White);
            let cursor_style = Style::default().fg(Color::White).bg(Color::DarkGray);
            if cx < chars.len() {
                let before: String = chars[..cx].iter().collect();
                let cursor_char: String = chars[cx].to_string();
                let after: String = chars[cx + 1..].iter().collect();
                lines.push(Line::from(vec![
                    Span::styled(format!("{:<12}", field.label()), label_style),
                    Span::raw(" "),
                    Span::styled(before, val_style),
                    Span::styled(cursor_char, cursor_style),
                    Span::styled(after, val_style),
                ]));
            } else {
                lines.push(Line::from(vec![
                    Span::styled(format!("{:<12}", field.label()), label_style),
                    Span::raw(" "),
                    Span::styled(visible.clone(), val_style),
                    Span::styled(" ", cursor_style),
                ]));
            }
            focus_line = Some(lines.len() as u16 - 1);
            focus_x = Some(13 + cursor_x);
        } else {
            lines.push(Line::from(vec![
                Span::styled(format!("{:<12}", field.label()), label_style),
                Span::raw(" "),
                Span::styled(display, Style::default().fg(Color::White)),
                Span::raw(" "),
            ]));
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
        let (key_disp, key_cursor_x) = if key_focused {
            scroll_to_cursor(&key_str, app.form.cursor, key_avail)
        } else {
            (key_str.clone(), 0)
        };
        let key_display_len = if cf.key.is_empty() && !key_focused {
            7
        } else {
            key_disp.chars().count() as u16
        };
        let val_avail = inner_w
            .saturating_sub(2 + key_display_len + 2 + 2 + mask_width)
            .max(8);
        let (val_disp, val_cursor_x) = if val_focused {
            scroll_to_cursor(&val_str, app.form.cursor, val_avail)
        } else {
            (val_str.clone(), 0)
        };

        // Build the line, splitting the focused field at the cursor position.
        let mut spans: Vec<Span> = vec![
            Span::raw("  "),
        ];
        if key_focused {
            let chars: Vec<char> = key_disp.chars().collect();
            let cx = key_cursor_x as usize;
            let cursor_style = Style::default().fg(ACCENT).bg(Color::DarkGray);
            if cx < chars.len() {
                let before: String = chars[..cx].iter().collect();
                let cursor_char: String = chars[cx].to_string();
                let after: String = chars[cx + 1..].iter().collect();
                spans.push(Span::styled(before, key_style));
                spans.push(Span::styled(cursor_char, cursor_style));
                spans.push(Span::styled(after, key_style));
            } else {
                spans.push(Span::styled(key_disp.clone(), key_style));
                spans.push(Span::styled(" ", cursor_style));
            }
        } else {
            spans.push(Span::styled(key_disp, key_style));
        }
        spans.push(Span::styled(": ", Style::default().fg(DIM)));
        if val_focused {
            let chars: Vec<char> = val_disp.chars().collect();
            let cx = val_cursor_x as usize;
            let cursor_style = Style::default().fg(Color::White).bg(Color::DarkGray);
            if cx < chars.len() {
                let before: String = chars[..cx].iter().collect();
                let cursor_char: String = chars[cx].to_string();
                let after: String = chars[cx + 1..].iter().collect();
                spans.push(Span::styled(before, val_style));
                spans.push(Span::styled(cursor_char, cursor_style));
                spans.push(Span::styled(after, val_style));
            } else {
                spans.push(Span::styled(val_disp.clone(), val_style));
                spans.push(Span::styled(" ", cursor_style));
            }
        } else {
            spans.push(Span::styled(val_disp, val_style));
        }
        spans.push(Span::raw("  "));
        spans.push(Span::styled(format!("[{}]", mask_label), mask_style));
        lines.push(Line::from(spans));

        if key_focused {
            focus_line = Some(lines.len() as u16 - 1);
            focus_x = Some(2 + key_cursor_x);
        } else if val_focused {
            focus_line = Some(lines.len() as u16 - 1);
            focus_x = Some(2 + key_display_len + 2 + val_cursor_x);
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

/// Show a window of `text` around `cursor` that fits in `avail` columns.
/// Returns `(visible_text, cursor_x_within_visible)`.
fn scroll_to_cursor(text: &str, cursor: usize, avail: u16) -> (String, u16) {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let avail = avail as usize;
    if avail == 0 || len <= avail {
        return (text.to_string(), cursor.min(len) as u16);
    }
    let cursor = cursor.min(len);
    // Keep cursor visible: start so that cursor is within [start, start+avail).
    let start = cursor
        .saturating_sub(avail.saturating_sub(1))
        .min(len.saturating_sub(avail));
    let end = (start + avail).min(len);
    let visible: String = chars[start..end].iter().collect();
    let cursor_x = (cursor - start) as u16;
    (visible, cursor_x)
}

/// Wrap a value string into chunks that fit within `avail` columns.
/// Character-based wrapping (suitable for secrets/tokens).
fn wrap_value(value: &str, avail: u16) -> Vec<String> {
    if avail == 0 {
        return vec![value.to_string()];
    }
    let avail = avail as usize;
    let chars: Vec<char> = value.chars().collect();
    if chars.is_empty() {
        return vec![String::new()];
    }
    chars.chunks(avail).map(|c| c.iter().collect()).collect()
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

    // Available width for values (label=12, marker="  [N]"≈5, borders=2).
    let inner_w = rect.width.saturating_sub(2);
    let val_avail = inner_w.saturating_sub(12 + 5);
    // For custom fields: prefix(2) + key(10) + ": "(2) + "  "(2) + mask_tag(9) + marker(5).
    let cf_val_avail = inner_w.saturating_sub(30);

    // [1] Name
    let (m, ms) = marker(0);
    let chunks = wrap_value(&d.name, val_avail);
    lines.push(Line::from(vec![
        Span::styled("Name        ", Style::default().fg(NORMAL)),
        Span::styled(&chunks[0], val_style(0, Style::default().fg(Color::White))),
        Span::styled(format!("  [{}]", m), ms),
    ]));
    for chunk in &chunks[1..] {
        lines.push(Line::from(vec![
            Span::raw("            "),
            Span::styled(chunk, val_style(0, Style::default().fg(Color::White))),
        ]));
    }
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
    let chunks = wrap_value(&secret_display, val_avail);
    lines.push(Line::from(vec![
        Span::styled("Secret      ", Style::default().fg(NORMAL)),
        Span::styled(&chunks[0], val_style(5, secret_base)),
        Span::styled(format!("  [{}]", m), ms),
    ]));
    for chunk in &chunks[1..] {
        lines.push(Line::from(vec![
            Span::raw("            "),
            Span::styled(chunk, val_style(5, secret_base)),
        ]));
    }

    lines.push(Line::from(""));

    // [7] Notes
    let (m, ms) = marker(6);
    let notes_str = if d.notes.is_empty() { "(none)" } else { &d.notes };
    let chunks = wrap_value(notes_str, val_avail);
    lines.push(Line::from(vec![
        Span::styled("Notes       ", Style::default().fg(NORMAL)),
        Span::styled(&chunks[0], val_style(6, Style::default())),
        Span::styled(format!("  [{}]", m), ms),
    ]));
    for chunk in &chunks[1..] {
        lines.push(Line::from(vec![
            Span::raw("            "),
            Span::styled(chunk, val_style(6, Style::default())),
        ]));
    }

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
            let chunks = wrap_value(&val_display, cf_val_avail);
            lines.push(Line::from(vec![
                Span::styled(prefix, Style::default().fg(ACCENT)),
                Span::styled(format!("{:<10}", cf.key), Style::default().fg(NORMAL)),
                Span::raw(": "),
                Span::styled(chunks[0].clone(), val_style(field_idx, Style::default().fg(Color::White))),
                Span::styled(mask_tag, Style::default().fg(NORMAL)),
                Span::styled(format!("  [{}]", m), ms),
            ]));
            let indent = format!("{}  {}  ", prefix, " ".repeat(10));
            for chunk in &chunks[1..] {
                lines.push(Line::from(vec![
                    Span::raw(indent.clone()),
                    Span::styled(chunk.clone(), val_style(field_idx, Style::default().fg(Color::White))),
                ]));
            }
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
        Span::styled(" ⇧+1 ", Style::default().bg(DIM).fg(Color::Black)),
        Span::raw(" quick-copy  "),
        Span::styled(" ↑/↓ ", Style::default().bg(DIM).fg(Color::Black)),
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

    // Masked password: one bullet per character, split at cursor.
    let label = "  Password: ";
    let label_len = label.len() as u16;
    let avail = rect.width.saturating_sub(2 + label_len);
    let (visible, cursor_x) = scroll_to_cursor(&app.reveal_password, app.reveal_cursor, avail);
    let visible_len = visible.chars().count();
    let cx = cursor_x as usize;
    let val_style = Style::default().fg(Color::White);
    let cursor_style = Style::default().fg(Color::White).bg(Color::DarkGray);
    let pw_line = if cx < visible_len {
        let before: String = "•".repeat(cx);
        let after: String = "•".repeat(visible_len - cx - 1);
        Line::from(vec![
            Span::styled(label, Style::default().fg(NORMAL)),
            Span::styled(before, val_style),
            Span::styled("•", cursor_style),
            Span::styled(after, val_style),
        ])
    } else {
        let before: String = "•".repeat(visible_len);
        Line::from(vec![
            Span::styled(label, Style::default().fg(NORMAL)),
            Span::styled(before, val_style),
            Span::styled(" ", cursor_style),
        ])
    };

    let (title, prompt) = if app.pending_edit {
        (" Confirm edit ", "  Editing requires master password")
    } else {
        (" Confirm reveal ", "  Reveal secret requires master password")
    };

    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            prompt,
            Style::default().fg(WARN).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        pw_line,
        Line::from(""),
        Line::from(vec![
            Span::styled(" Enter ", Style::default().bg(DIM).fg(Color::Black)),
            Span::raw(" confirm  "),
            Span::styled(" Esc ", Style::default().bg(DIM).fg(Color::Black)),
            Span::raw(" cancel"),
        ]),
    ];

    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        title,
        Style::default().fg(WARN).add_modifier(Modifier::BOLD),
    ));
    let para = Paragraph::new(lines).block(block);
    f.render_widget(para, rect);

    // Place cursor at the highlighted position.
    let cx = rect.x + 1 + label_len + cursor_x;
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
        ("s", "Settings (change password, manage tokens)"),
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

fn draw_settings(f: &mut Frame, app: &App) {
    use crate::tui::SettingsTab;
    let area = f.area();
    let rect = centered_rect(area, 65, 70);
    f.render_widget(Clear, rect);

    // Tab bar.
    let tabs = [("Password", SettingsTab::Password), ("Tokens", SettingsTab::Tokens), ("Info", SettingsTab::Info)];
    let tab_line = Line::from(
        tabs.iter()
            .map(|(label, tab)| {
                let style = if *tab == app.settings_tab {
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
                } else {
                    Style::default().fg(DIM)
                };
                Span::styled(format!(" {} ", label), style)
            })
            .collect::<Vec<_>>(),
    );

    let inner = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(rect);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            " Settings ",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ));
    f.render_widget(Paragraph::new(tab_line).block(block.clone()), inner[0]);

    let content_block = Block::default().borders(Borders::LEFT | Borders::RIGHT | Borders::BOTTOM);
    let content_inner = content_block.inner(inner[1]);
    f.render_widget(content_block, inner[1]);

    match app.settings_tab {
        SettingsTab::Password => draw_settings_password(f, app, content_inner),
        SettingsTab::Tokens => draw_settings_tokens(f, app, content_inner),
        SettingsTab::Info => draw_settings_info(f, app, content_inner),
    }
}

fn draw_settings_password(f: &mut Frame, app: &App, area: Rect) {
    let inner = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Length(3), Constraint::Min(1)])
        .split(area);

    let new_display = mask_password(&app.settings_new_pw);
    let confirm_display = mask_password(&app.settings_confirm_pw);

    // New password field.
    let new_line = build_password_line("New password:      ", &new_display, app.settings_pw_field == 0, app.settings_pw_cursor);
    f.render_widget(Paragraph::new(new_line).block(Block::default().borders(Borders::NONE)), inner[0]);

    // Confirm password field.
    let confirm_line = build_password_line("Confirm password:  ", &confirm_display, app.settings_pw_field == 1, app.settings_pw_cursor);
    f.render_widget(Paragraph::new(confirm_line), inner[1]);

    // Hints.
    let hints = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("↑/↓", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
            Span::raw(": switch field  "),
            Span::styled("←/→", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
            Span::raw(": move cursor  "),
            Span::styled("Enter", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
            Span::raw(": change password"),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Note: changing the password revokes all existing tokens.",
            Style::default().fg(DIM),
        )),
    ];
    f.render_widget(Paragraph::new(Text::from(hints)), inner[2]);
}

fn build_password_line(label: &str, display: &str, focused: bool, cursor: usize) -> Line<'static> {
    let label_style = Style::default().fg(NORMAL);
    let val_style = if focused {
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(NORMAL)
    };
    if !focused {
        return Line::from(vec![
            Span::styled(label.to_string(), label_style),
            Span::styled(display.to_string(), val_style),
        ]);
    }

    let chars: Vec<char> = display.chars().collect();
    let cursor = cursor.min(chars.len());
    let before: String = chars[..cursor].iter().collect();

    if cursor < chars.len() {
        // Cursor is on a character — highlight it with a background.
        let cursor_char: String = chars[cursor].to_string();
        let after: String = chars[cursor + 1..].iter().collect();
        Line::from(vec![
            Span::styled(label.to_string(), label_style),
            Span::styled(before, val_style),
            Span::styled(cursor_char, val_style.bg(Color::DarkGray)),
            Span::styled(after, val_style),
        ])
    } else {
        // Cursor is at the end — show a highlighted space as the cursor.
        Line::from(vec![
            Span::styled(label.to_string(), label_style),
            Span::styled(before, val_style),
            Span::styled(" ", val_style.bg(Color::DarkGray)),
        ])
    }
}

fn mask_password(s: &str) -> String {
    "•".repeat(s.chars().count())
}

fn draw_settings_tokens(f: &mut Frame, app: &App, area: Rect) {
    let inner = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(3), Constraint::Length(3), Constraint::Length(3)])
        .split(area);

    // Column headers.
    let header = Line::from(vec![
        Span::styled("  ID    ", Style::default().fg(DIM).add_modifier(Modifier::BOLD)),
        Span::styled("Label           ", Style::default().fg(DIM).add_modifier(Modifier::BOLD)),
        Span::styled("Perms   ", Style::default().fg(DIM).add_modifier(Modifier::BOLD)),
        Span::styled("Created", Style::default().fg(DIM).add_modifier(Modifier::BOLD)),
    ]);
    f.render_widget(Paragraph::new(header), inner[0]);

    // Token list.
    if app.settings_tokens.is_empty() {
        f.render_widget(
            Paragraph::new(Text::from(vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  No tokens. Press c to create one.",
                    Style::default().fg(DIM),
                )),
            ])),
            inner[1],
        );
    } else {
        let items: Vec<ListItem> = app
            .settings_tokens
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let style = if i == app.settings_token_sel {
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(NORMAL)
                };
                ListItem::new(Line::from(vec![
                    Span::styled(format!("  #{:<3} ", t.id), style),
                    Span::styled(format!("{:<16} ", truncate_str(&t.label, 16)), style),
                    Span::styled(fmt_ts(t.created_at), Style::default().fg(DIM)),
                ]))
            })
            .collect();
        let list = List::new(items)
            .block(Block::default().borders(Borders::NONE));
        f.render_widget(list, inner[1]);
    }

    // Label input or hints.
    if app.settings_token_creating {
        let label_display = app.settings_token_label.clone();
        let chars: Vec<char> = label_display.chars().collect();
        let cursor = app.settings_token_label_cursor.min(chars.len());
        let before: String = chars[..cursor].iter().collect();
        let val_style = Style::default().fg(NORMAL);
        let cursor_style = Style::default().fg(NORMAL).bg(Color::DarkGray);
        let line = if cursor < chars.len() {
            let cursor_char: String = chars[cursor].to_string();
            let after: String = chars[cursor + 1..].iter().collect();
            Line::from(vec![
                Span::styled("Label: ", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
                Span::styled(before, val_style),
                Span::styled(cursor_char, cursor_style),
                Span::styled(after, val_style),
            ])
        } else {
            Line::from(vec![
                Span::styled("Label: ", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
                Span::styled(before, val_style),
                Span::styled(" ", cursor_style),
            ])
        };
        f.render_widget(Paragraph::new(line), inner[2]);
        let hint = Line::from(Span::styled(
            "  Enter to create, Esc to cancel",
            Style::default().fg(DIM),
        ));
        f.render_widget(Paragraph::new(hint), inner[3]);
    } else {
        let hints = Line::from(vec![
            Span::styled("c/n", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
            Span::raw(":create  "),
            Span::styled("x/Del", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
            Span::raw(":revoke  "),
            Span::styled("↑/↓", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
            Span::raw(":navigate"),
        ]);
        f.render_widget(Paragraph::new(hints), inner[2]);
        if !app.vault.permission().is_full() {
            let warn = Line::from(Span::styled(
                "  Basic mode: token creation is disabled.",
                Style::default().fg(WARN),
            ));
            f.render_widget(Paragraph::new(warn), inner[3]);
        }
    }
}

fn draw_settings_info(f: &mut Frame, app: &App, area: Rect) {
    let cred_count = app.all.len();
    let token_count = app.settings_tokens.len();
    let perm = if app.vault.permission().is_full() { "Full (master)" } else { "Basic (token)" };

    let lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("Vault path:       ", Style::default().fg(NORMAL)),
            Span::styled(app.vault_path.display().to_string(), Style::default().fg(ACCENT)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("Permission:       ", Style::default().fg(NORMAL)),
            Span::styled(perm, Style::default().fg(ACCENT)),
        ]),
        Line::from(vec![
            Span::styled("Credentials:      ", Style::default().fg(NORMAL)),
            Span::styled(cred_count.to_string(), Style::default().fg(ACCENT)),
        ]),
        Line::from(vec![
            Span::styled("Tokens:           ", Style::default().fg(NORMAL)),
            Span::styled(token_count.to_string(), Style::default().fg(ACCENT)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("Environments:     ", Style::default().fg(NORMAL)),
            Span::styled(app.envs.join(", "), Style::default().fg(DIM)),
        ]),
        Line::from(vec![
            Span::styled("Projects:         ", Style::default().fg(NORMAL)),
            Span::styled(app.projects.join(", "), Style::default().fg(DIM)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "  Tab to switch tabs. Esc to close.",
            Style::default().fg(DIM),
        )),
    ];
    f.render_widget(Paragraph::new(Text::from(lines)), area);
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let chars: Vec<char> = s.chars().take(max - 1).collect();
        format!("{}…", chars.iter().collect::<String>())
    }
}
fn fmt_ts(ts: i64) -> String {
    use chrono::DateTime;
    match DateTime::from_timestamp(ts, 0) {
        Some(dt) => dt.format("%Y-%m-%d %H:%M UTC").to_string(),
        None => ts.to_string(),
    }
}
