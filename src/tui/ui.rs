//! Rendering. Reads `App`, writes to the frame, changes nothing.

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, Wrap};

use super::app::{App, Mode, Pane};
use crate::model::{JobRow, Stage};

/// Colors are chosen to stay legible on both light and dark terminals:
/// no bright-on-bright, and never color as the *only* carrier of meaning —
/// the stage column shows its own text, the score its own number.
fn stage_style(stage: Option<Stage>) -> Style {
    match stage {
        Some(Stage::PreSeed) | Some(Stage::Seed) => Style::default().fg(Color::Green),
        Some(Stage::SeriesA) | Some(Stage::SeriesB) => Style::default().fg(Color::Cyan),
        Some(Stage::SeriesC) | Some(Stage::Growth) => Style::default().fg(Color::Blue),
        Some(Stage::Public) | Some(Stage::BigTech) => Style::default().fg(Color::DarkGray),
        None => Style::default().fg(Color::DarkGray),
    }
}

fn score_style(score: Option<i64>) -> Style {
    match score {
        Some(s) if s >= 80 => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        Some(s) if s >= 60 => Style::default().fg(Color::Yellow),
        Some(_) => Style::default().fg(Color::DarkGray),
        None => Style::default().fg(Color::DarkGray),
    }
}

fn recommendation_style(rec: &str) -> Style {
    match rec {
        "apply" => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        "consider" => Style::default().fg(Color::Yellow),
        "skip" => Style::default().fg(Color::DarkGray),
        _ => Style::default().fg(Color::DarkGray),
    }
}

fn my_status_style(status: Option<&str>) -> Style {
    match status {
        Some("applied") => Style::default().fg(Color::Blue),
        Some("interview") => Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
        Some("rejected") => Style::default().fg(Color::Red),
        Some("skipped") | Some("silence") => Style::default().fg(Color::DarkGray),
        _ => Style::default(),
    }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Min(5),    // body
            Constraint::Length(1), // status
            Constraint::Length(1), // keys
        ])
        .split(f.area());

    draw_header(f, chunks[0], app);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(chunks[1]);

    draw_table(f, body[0], app);
    draw_detail(f, body[1], app);

    draw_status(f, chunks[2], app);
    draw_keys(f, chunks[3], app);

    if app.mode == Mode::Help {
        draw_help(f, f.area());
    }
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let mut spans = vec![
        Span::styled(
            " jobsearch ",
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::Black)
                .bg(Color::Cyan),
        ),
        Span::raw(format!(
            " {} shown / {} total · {} startup · {} unscored ",
            app.view.len(),
            app.rows.len(),
            app.startup_count(),
            app.unscored_count()
        )),
    ];

    // The startup badge is the one piece of global state worth a
    // permanently visible indicator: every count on this bar means
    // something different depending on whether it's on.
    if app.startup_only {
        spans.push(Span::styled(
            format!(" STARTUP {} ", app.startup_policy.describe()),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ));
    } else {
        spans.push(Span::styled(
            " ALL STAGES ",
            Style::default().fg(Color::Black).bg(Color::DarkGray),
        ));
    }

    spans.push(Span::styled(
        format!(
            "  ai:{} mine:{} sort:{}{}",
            app.ai_filter.label(),
            app.my_filter.label(),
            app.sort.label(),
            if app.sort_desc { "↓" } else { "↑" }
        ),
        Style::default().fg(Color::DarkGray),
    ));

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_table(f: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.pane == Pane::List;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(if focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        })
        .title(if app.search.is_empty() {
            " jobs ".to_string()
        } else {
            format!(" jobs · /{} ", app.search)
        });

    if app.view.is_empty() {
        let msg = if app.rows.is_empty() {
            "No jobs stored yet.\n\nPress r to run a fetch, or run `jobsearch fetch` first."
        } else {
            "Nothing matches the current filters.\n\n\
             s toggles startup mode · f cycles the AI filter · m cycles your status filter"
        };
        f.render_widget(
            Paragraph::new(msg)
                .block(block)
                .style(Style::default().fg(Color::DarkGray))
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }

    let header = Row::new(vec![
        Cell::from("scr"),
        Cell::from("company"),
        Cell::from("title"),
        Cell::from("stg"),
        Cell::from("rec"),
        Cell::from("mine"),
    ])
    .style(
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );

    let rows: Vec<Row> = app
        .view
        .iter()
        .map(|&i| {
            let r = &app.rows[i];
            Row::new(vec![
                Cell::from(
                    r.match_score
                        .map(|s| format!("{s:3}"))
                        .unwrap_or_else(|| "  –".into()),
                )
                .style(score_style(r.match_score)),
                Cell::from(r.company.clone()),
                Cell::from(r.title.clone()),
                Cell::from(r.stage.map(Stage::short).unwrap_or("?")).style(stage_style(r.stage)),
                Cell::from(r.ai_status().to_string()).style(recommendation_style(r.ai_status())),
                Cell::from(r.my_status.clone().unwrap_or_default())
                    .style(my_status_style(r.my_status.as_deref())),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(3),
        Constraint::Percentage(26),
        Constraint::Fill(1),
        Constraint::Length(4),
        Constraint::Length(8),
        Constraint::Length(9),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(block)
        .row_highlight_style(
            Style::default()
                .add_modifier(Modifier::REVERSED)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("");

    app.sync_table_state();
    let mut state = std::mem::take(&mut app.table_state);
    f.render_stateful_widget(table, area, &mut state);
    // Take it back with whatever offset ratatui just computed, so the
    // next frame scrolls from here instead of from the top.
    app.table_state = state;
}

fn draw_detail(f: &mut Frame, area: Rect, app: &App) {
    let focused = app.pane == Pane::Detail;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(if focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        })
        .title(" detail ");

    let Some(row) = app.selected_row() else {
        f.render_widget(
            Paragraph::new("Nothing selected.")
                .block(block)
                .style(Style::default().fg(Color::DarkGray)),
            area,
        );
        return;
    };

    f.render_widget(
        Paragraph::new(detail_lines(row))
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((app.detail_scroll, 0)),
        area,
    );
}

/// Build the detail pane's content.
///
/// Ordering is the point: what the AI concluded comes first, then the
/// evidence for it, and the raw JD last. That puts the thing you'd act on
/// at the top of a pane you may never scroll, rather than making you page
/// past a full job description to find the score.
fn detail_lines(row: &JobRow) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled(
            row.title.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            row.company.clone(),
            Style::default().fg(Color::Cyan),
        )),
    ];

    let mut meta = vec![Span::raw(row.location.clone())];
    if let Some(stage) = row.stage {
        meta.push(Span::raw(" · "));
        meta.push(Span::styled(
            stage.as_str().to_string(),
            stage_style(Some(stage)),
        ));
        if let Some(h) = row.headcount {
            meta.push(Span::raw(format!(" · ~{h} people")));
        }
        // Showing provenance next to the stage is what keeps startup mode
        // honest: "seed (ai)" invites a second look in a way "seed" does
        // not, and the three sources really do differ in reliability.
        if let Some(src) = row.stage_source {
            meta.push(Span::styled(
                format!(" ({})", src.as_str()),
                Style::default().fg(Color::DarkGray),
            ));
        }
    } else {
        meta.push(Span::styled(
            " · stage unknown",
            Style::default().fg(Color::DarkGray),
        ));
    }
    lines.push(Line::from(meta));

    let mut sub = Vec::new();
    if !row.source.is_empty() {
        sub.push(Span::styled(
            format!("via {}", row.source),
            Style::default().fg(Color::DarkGray),
        ));
    }
    if let Some(posted) = &row.posted_at {
        sub.push(Span::styled(
            format!(" · posted {}", &posted[..posted.len().min(10)]),
            Style::default().fg(Color::DarkGray),
        ));
    }
    if !sub.is_empty() {
        lines.push(Line::from(sub));
    }
    lines.push(Line::from(Span::styled(
        row.url.clone(),
        Style::default()
            .fg(Color::Blue)
            .add_modifier(Modifier::UNDERLINED),
    )));
    lines.push(Line::from(""));

    match (row.match_score, row.recommendation.as_deref()) {
        (Some(score), Some(rec)) => lines.push(Line::from(vec![
            Span::styled(format!("{score}/100  "), score_style(Some(score))),
            Span::styled(rec.to_uppercase(), recommendation_style(rec)),
        ])),
        _ => lines.push(Line::from(Span::styled(
            "not yet scored — press e to run the AI evaluation",
            Style::default().fg(Color::DarkGray),
        ))),
    }

    if let Some(status) = &row.my_status {
        lines.push(Line::from(vec![
            Span::styled("your status: ", Style::default().fg(Color::DarkGray)),
            Span::styled(status.clone(), my_status_style(Some(status))),
        ]));
    }
    if let Some(notes) = &row.notes {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "NOTES",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )));
        for l in notes.lines() {
            lines.push(Line::from(l.to_string()));
        }
    }

    for (label, body) in [
        ("STRENGTHS", &row.transferable_strengths),
        ("GAPS", &row.genuine_gaps),
        ("RISKS", &row.risk_factors),
    ] {
        if let Some(text) = body.as_deref().filter(|t| !t.is_empty()) {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                label,
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )));
            for l in text.lines() {
                lines.push(Line::from(l.to_string()));
            }
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "── job description ──",
        Style::default().fg(Color::DarkGray),
    )));
    let body = crate::filters::collapse_whitespace(&crate::filters::strip_html(&row.description));
    if body.is_empty() {
        lines.push(Line::from(Span::styled(
            "(no description stored)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for l in body.lines() {
            lines.push(Line::from(l.to_string()));
        }
    }

    lines
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let line = match app.mode {
        Mode::Search => Line::from(vec![
            Span::styled(
                "/",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(app.input.clone()),
            Span::styled("█", Style::default().fg(Color::Cyan)),
        ]),
        Mode::Notes => Line::from(vec![
            Span::styled(
                "note: ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(app.input.clone()),
            Span::styled("█", Style::default().fg(Color::Cyan)),
        ]),
        _ if app.task.running => {
            let pct = if app.task.total > 0 {
                format!(" {}/{}", app.task.done, app.task.total)
            } else {
                String::new()
            };
            let mut spans = vec![
                Span::styled(
                    format!(" {} ", app.task.label),
                    Style::default().fg(Color::Black).bg(Color::Yellow),
                ),
                Span::raw(format!("{pct} {}", app.task.detail)),
            ];
            if app.task.failures > 0 {
                spans.push(Span::styled(
                    format!("  {} failed", app.task.failures),
                    Style::default().fg(Color::Red),
                ));
            }
            Line::from(spans)
        }
        _ => Line::from(Span::styled(
            app.status.clone(),
            Style::default().fg(Color::DarkGray),
        )),
    };
    f.render_widget(Paragraph::new(line), area);
}

fn draw_keys(f: &mut Frame, area: Rect, app: &App) {
    let keys = match app.mode {
        Mode::Search | Mode::Notes => "enter save · esc cancel",
        Mode::Help => "any key to close",
        Mode::Normal => {
            "j/k move · tab pane · s startup · / search · f ai · m mine · S sort · \
             1-5 status · 0 clear · n note · o open · r fetch · e eval · ? help · q quit"
        }
    };
    f.render_widget(
        Paragraph::new(Span::styled(keys, Style::default().fg(Color::DarkGray))),
        area,
    );
}

fn draw_help(f: &mut Frame, area: Rect) {
    let text = vec![
        Line::from(Span::styled(
            "  jobsearch — keys",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("  MOVE     j/k or ↑/↓   move      g/G  top/bottom"),
        Line::from("           tab           switch pane   J/K  scroll detail"),
        Line::from(""),
        Line::from("  FILTER   s   startup mode on/off"),
        Line::from("           f   AI verdict: all/apply/consider/skip/unscored"),
        Line::from("           m   your status: all/untouched/applied/..."),
        Line::from("           /   search company, title and location"),
        Line::from("           S   cycle sort key      R  reverse sort"),
        Line::from(""),
        Line::from("  TRIAGE   1 applied   2 interview   3 rejected"),
        Line::from("           4 skipped   5 silence     0 clear"),
        Line::from("           n   edit notes          o  open in browser"),
        Line::from(""),
        Line::from("  RUN      r   fetch new jobs from every configured source"),
        Line::from("           e   score unevaluated jobs with Claude (costs money)"),
        Line::from("           F5  reload from the database"),
        Line::from(""),
        Line::from(Span::styled(
            "  Startup mode is a view, not a fetch setting — everything is",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(Span::styled(
            "  always stored, so toggling it never needs a re-fetch.",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  press any key to close",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let popup = centered_rect(72, 82, area);
    f.render_widget(Clear, popup);
    f.render_widget(
        Paragraph::new(text).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" help "),
        ),
        popup,
    );
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(v[1])[1]
}
