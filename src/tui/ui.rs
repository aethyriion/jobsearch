//! Rendering. Reads `App`, writes to the frame, changes nothing.

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, Wrap};

use super::app::{App, Mode, Pane};
use crate::model::{Channel, JobRow, Seniority, Stage};

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

/// Junior and intern stand out because they're the rungs you'd be
/// deliberately looking for when you widen the search; manager is dimmed
/// because it's a different track rather than a level.
fn level_style(level: Option<Seniority>) -> Style {
    match level {
        Some(Seniority::Intern) | Some(Seniority::Junior) => Style::default().fg(Color::Magenta),
        Some(Seniority::Mid) => Style::default().fg(Color::Cyan),
        Some(Seniority::Senior) | Some(Seniority::Staff) => Style::default().fg(Color::Blue),
        Some(Seniority::Manager) => Style::default().fg(Color::DarkGray),
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

/// Odds get their own ramp rather than reusing `score_style`: the numbers
/// mean different things, and 45 is a perfectly ordinary callback estimate
/// where 45 as a fit score is not.
fn odds_style(odds: Option<i64>) -> Style {
    match odds {
        Some(o) if o >= 60 => Style::default().fg(Color::Green),
        Some(o) if o >= 35 => Style::default().fg(Color::Yellow),
        Some(_) => Style::default().fg(Color::Red),
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
        Some("recruiter") => Style::default().fg(Color::Cyan),
        Some("rejected") => Style::default().fg(Color::Red),
        Some("skipped") | Some("silence") => Style::default().fg(Color::DarkGray),
        _ => Style::default(),
    }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    // The form is a page, not an overlay: it needs the vertical room for a
    // pasted job description, and there is nothing useful to see behind it.
    if app.mode == Mode::Form {
        draw_form(f, f.area(), app);
        return;
    }

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
        // The list carries eight columns now; the detail pane still reads
        // fine at 36% and the extra width is what keeps the title legible.
        .constraints([Constraint::Percentage(64), Constraint::Percentage(36)])
        .split(chunks[1]);

    draw_table(f, body[0], app);
    draw_detail(f, body[1], app);

    draw_status(f, chunks[2], app);
    draw_keys(f, chunks[3], app);

    if app.mode == Mode::Help {
        draw_help(f, f.area());
    }
    if app.mode == Mode::ConfirmDelete {
        draw_confirm_delete(f, f.area(), app);
    }
    if matches!(
        app.mode,
        Mode::ConfirmDismissJob | Mode::ConfirmDismissCompany
    ) {
        draw_confirm_dismiss(f, f.area(), app);
    }
    if app.mode == Mode::TechPicker {
        draw_tech_picker(f, f.area(), app);
    }
    if app.mode == Mode::ConfirmEvaluateAll {
        draw_confirm_evaluate(f, f.area(), app);
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
            " {} shown / {} total · {} startup · {} unscored · {} yours ",
            app.view.len(),
            app.rows.len(),
            app.startup_count(),
            app.unscored_count(),
            app.manual_count()
        )),
    ];
    if app.dismissed_count() > 0 {
        spans.push(Span::styled(
            format!("· {} hidden ", app.dismissed_count()),
            Style::default().fg(Color::DarkGray),
        ));
    }

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

    // A running tally of where your applications stand, so the tracker
    // state is visible without switching filters to count them.
    let counts = app.status_counts();
    if !counts.is_empty() {
        spans.push(Span::raw(" "));
        for (i, (status, n)) in counts.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" ", Style::default().fg(Color::DarkGray)));
            }
            spans.push(Span::styled(
                format!("{n} {status}"),
                my_status_style(Some(status)),
            ));
        }
    }

    // Stale rows are the only count on this bar that's a to-do rather
    // than a statistic, so it gets a color the rest of the bar doesn't
    // use and sits next to the status tally it's drawn from.
    if app.stale_count() > 0 {
        spans.push(Span::styled(
            format!("  {} stale ", app.stale_count()),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Red)
                .add_modifier(Modifier::BOLD),
        ));
    }

    spans.push(Span::styled(
        format!(
            "  ai:{} mine:{} lvl:{} tech:{} sort:{}{}",
            app.ai_filter.label(),
            app.my_filter.label(),
            app.level_filter.label(),
            app.tech_label(),
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
        Cell::from(""),
        Cell::from(""),
        Cell::from("company"),
        Cell::from("title"),
        Cell::from("lvl"),
        Cell::from("stg"),
        Cell::from("stack"),
        Cell::from("rec"),
        Cell::from("seen"),
        Cell::from("mine · age"),
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
                // One character marking a row you entered yourself. It
                // earns its column: a hand-entered row is exempt from
                // startup mode and from refilter, so knowing which rows
                // those are explains why they're on screen.
                // One character carrying both origin and visibility:
                // `+` you entered it, `-` it's hidden, `=` both.
                Cell::from(match (r.is_manual(), r.dismissed) {
                    (true, true) => "=",
                    (true, false) => "+",
                    (false, true) => "-",
                    (false, false) => " ",
                })
                .style(if r.dismissed {
                    Style::default().fg(Color::DarkGray)
                } else {
                    Style::default().fg(Color::Magenta)
                }),
                // Agency listings are the ones worth spotting from across
                // the list: a different process, and often the same role
                // three times under different URLs.
                Cell::from(r.channel.map(Channel::short).unwrap_or(" ")).style(if r.is_agency() {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                }),
                Cell::from(r.company.clone()).style(if r.dismissed {
                    Style::default().fg(Color::DarkGray)
                } else {
                    Style::default()
                }),
                Cell::from(r.title.clone()).style(if r.dismissed {
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::CROSSED_OUT)
                } else {
                    Style::default()
                }),
                Cell::from(r.seniority().map(Seniority::short).unwrap_or("–"))
                    .style(level_style(r.seniority())),
                Cell::from(r.stage.map(Stage::short).unwrap_or("?")).style(stage_style(r.stage)),
                Cell::from(r.tech_summary_pinned(2, app.tech_filter.as_ref()))
                    .style(Style::default().fg(Color::Green)),
                Cell::from(r.ai_status().to_string()).style(recommendation_style(r.ai_status())),
                // How likely the application is to be read at all — the
                // number that decides whether a good fit is worth the
                // twenty minutes.
                Cell::from(
                    r.callback_odds
                        .map(|o| format!("{o:3}"))
                        .unwrap_or_else(|| "  –".into()),
                )
                .style(odds_style(r.callback_odds)),
                // Status and age share one cell rather than taking two.
                // They're read together — "applied, and it's been three
                // weeks" is one thought — and the list is already wide.
                // Red on the age means it's past the staleness threshold
                // for that status: time to chase it or call it dead.
                Cell::from(Line::from(vec![
                    Span::styled(
                        r.my_status.clone().unwrap_or_default(),
                        my_status_style(r.my_status.as_deref()),
                    ),
                    Span::raw(" "),
                    Span::styled(
                        r.age_label().unwrap_or_default(),
                        if app.is_stale(r) {
                            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(Color::DarkGray)
                        },
                    ),
                ])),
            ])
        })
        .collect();

    // Company and title share the leftover space 2:3 rather than company
    // taking a fixed percentage. With nine fixed-width columns either side
    // of them, a percentage left the title with about eight characters —
    // enough to show "Staff SR" and not which role it was.
    let widths = [
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(2),
        Constraint::Fill(3),
        Constraint::Length(3),
        Constraint::Length(4),
        Constraint::Length(14),
        Constraint::Length(8),
        Constraint::Length(4),
        Constraint::Length(13),
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
        Paragraph::new(detail_lines(row, app.staleness_days(row)))
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
fn detail_lines(row: &JobRow, stale_days: Option<i64>) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    push_header(&mut lines, row);
    push_scores(&mut lines, row);
    push_status(&mut lines, row, stale_days);
    push_notes(&mut lines, row);
    push_stack(&mut lines, row);
    push_description(&mut lines, row);
    lines
}

fn push_header(lines: &mut Vec<Line<'static>>, row: &JobRow) {
    lines.push(Line::from(Span::styled(
        row.title.clone(),
        Style::default().add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::styled(
        row.company.clone(),
        Style::default().fg(Color::Cyan),
    )));

    let mut meta = vec![Span::raw(row.location.clone())];
    match row.seniority() {
        Some(level) => {
            meta.push(Span::raw(" · "));
            meta.push(Span::styled(
                level.as_str().to_string(),
                level_style(Some(level)),
            ));
        }
        None => meta.push(Span::styled(
            " · level not stated",
            Style::default().fg(Color::DarkGray),
        )),
    }
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
                format!(" ({})", src.label()),
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

    if row.dismissed {
        lines.push(Line::from(vec![
            Span::styled(
                match row.dismissed_scope {
                    Some("company") => " HIDDEN — whole employer ",
                    _ => " HIDDEN ",
                },
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                match &row.dismissed_reason {
                    Some(r) if !r.is_empty() => format!("  {r}   (u restores)"),
                    _ => "  u restores".to_string(),
                },
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }

    if let Some(channel) = row.channel {
        lines.push(Line::from(vec![
            Span::styled(
                match channel {
                    Channel::Agency => " STAFFING AGENCY ",
                    Channel::Referral => " REFERRAL ",
                    Channel::Inbound => " RECRUITER OUTREACH ",
                    Channel::Direct => " DIRECT ",
                },
                if channel == Channel::Agency {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            ),
            Span::styled(
                row.channel_source
                    .map(|s| format!("  ({})", s.label()))
                    .unwrap_or_default(),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }

    let mut sub = Vec::new();
    if row.is_manual() {
        sub.push(Span::styled(
            "entered by hand",
            Style::default().fg(Color::Magenta),
        ));
    } else if !row.source.is_empty() {
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
}

fn push_scores(lines: &mut Vec<Line<'static>>, row: &JobRow) {
    match (row.match_score, row.recommendation.as_deref()) {
        (Some(score), Some(rec)) => {
            lines.push(Line::from(vec![
                Span::styled(format!("{score}/100  "), score_style(Some(score))),
                Span::styled(rec.to_uppercase(), recommendation_style(rec)),
            ]));
            // The two halves behind the headline number. Worth showing
            // separately because they point at different actions: a low
            // qualification score means don't bother, a low odds score
            // means find a referral or a back channel.
            if row.qualification_match.is_some() || row.callback_odds.is_some() {
                let mut split = Vec::new();
                if let Some(q) = row.qualification_match {
                    split.push(Span::styled(
                        "qualified ",
                        Style::default().fg(Color::DarkGray),
                    ));
                    split.push(Span::styled(format!("{q}"), score_style(Some(q))));
                }
                if let Some(o) = row.callback_odds {
                    if !split.is_empty() {
                        split.push(Span::styled("   ", Style::default()));
                    }
                    split.push(Span::styled(
                        "seen by a human ",
                        Style::default().fg(Color::DarkGray),
                    ));
                    split.push(Span::styled(format!("{o}"), odds_style(Some(o))));
                }
                lines.push(Line::from(split));
            }
            if let Some(notes) = row.callback_notes.as_deref().filter(|n| !n.is_empty()) {
                for l in notes.lines() {
                    lines.push(Line::from(Span::styled(
                        l.to_string(),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
            }
        }
        _ => lines.push(Line::from(Span::styled(
            "not yet scored — press e to run the AI evaluation",
            Style::default().fg(Color::DarkGray),
        ))),
    }
}

fn push_status(lines: &mut Vec<Line<'static>>, row: &JobRow, stale_days: Option<i64>) {
    if let Some(status) = &row.my_status {
        let mut head = vec![
            Span::styled("your status: ", Style::default().fg(Color::DarkGray)),
            Span::styled(status.clone(), my_status_style(Some(status))),
        ];
        if let Some(phrase) = row.age_phrase() {
            head.push(Span::styled(
                if phrase == "today" {
                    "  ·  set today".to_string()
                } else {
                    format!("  ·  set {phrase}")
                },
                Style::default().fg(Color::DarkGray),
            ));
        }
        // How long it's been in play overall, which is a different
        // question from how long it's been in this status: an interview
        // set yesterday off an application from two months ago reads very
        // differently from one off an application from last week.
        if let Some(applied) = row.first_reached("applied").map(str::to_string) {
            if status != "applied" {
                if let Some(days) = crate::model::days_since(&applied) {
                    head.push(Span::styled(
                        format!("  ·  applied {}", crate::model::phrase_age(days)),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
            }
        } else if let Some(days) = row.tracked_age_days() {
            head.push(Span::styled(
                format!("  ·  tracked {}", crate::model::phrase_age(days)),
                Style::default().fg(Color::DarkGray),
            ));
        }
        lines.push(Line::from(head));

        // The nudge, not just the number: a threshold crossed is the only
        // thing here that asks you to do something.
        if let Some(over) = stale_days {
            lines.push(Line::from(Span::styled(
                format!(
                    " NO MOVEMENT — {} past the {status} threshold; chase it or press 6 for silence ",
                    if over == 0 {
                        "right at".to_string()
                    } else {
                        format!("{over}d")
                    }
                ),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Red)
                    .add_modifier(Modifier::BOLD),
            )));
        }

        // The whole path, not just where it landed. "applied 4 Aug,
        // recruiter 2 Sep, still nothing" is a different situation from
        // "recruiter yesterday", and the current status alone can't tell
        // them apart.
        if row.status_timeline.len() > 1 {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "TIMELINE",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )));
            for (st, at) in &row.status_timeline {
                let label = st.clone().unwrap_or_else(|| "cleared".into());
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  {:<10}", &at[..at.len().min(10)]),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(format!("{label:<10}"), my_status_style(st.as_deref())),
                    Span::styled(
                        crate::model::days_since(at)
                            .map(|d| format!("  {}", crate::model::compact_age(d)))
                            .unwrap_or_default(),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
            }
        }
    }
}

fn push_notes(lines: &mut Vec<Line<'static>>, row: &JobRow) {
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
}

fn push_stack(lines: &mut Vec<Line<'static>>, row: &JobRow) {
    if !row.tech.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "STACK",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )));
        // Grouped by category so "which cloud" and "which language" are
        // each answerable at a glance rather than from a flat soup.
        let mut category = String::new();
        let mut line: Vec<Span> = Vec::new();
        for tag in &row.tech {
            if tag.category != category {
                if !line.is_empty() {
                    lines.push(Line::from(std::mem::take(&mut line)));
                }
                category = tag.category.clone();
                line.push(Span::styled(
                    format!("{category:<14}"),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            line.push(Span::styled(
                format!("{} ", tag.name),
                Style::default().fg(Color::Green),
            ));
        }
        if !line.is_empty() {
            lines.push(Line::from(line));
        }
    }
}

fn push_description(lines: &mut Vec<Line<'static>>, row: &JobRow) {
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
        Mode::Search | Mode::Notes => "enter save · esc cancel".to_string(),
        Mode::Help => "any key to close".to_string(),
        // Drawn by draw_form; the form never reaches this footer.
        Mode::Form => String::new(),
        Mode::ConfirmDelete => "y delete · any other key cancel".to_string(),
        Mode::ConfirmDismissJob | Mode::ConfirmDismissCompany => {
            "y hide it · any other key cancel".to_string()
        }
        Mode::ConfirmEvaluateAll => "y score them · any other key cancel".to_string(),
        Mode::TechPicker => {
            "type to filter · ↑/↓ move · enter select · 0 clear · esc cancel".to_string()
        }
        // Every binding that isn't discoverable by looking at the screen
        // belongs here, and the whole line has to fit — a hint that gets
        // truncated off the right edge is the same as no hint. So the
        // narrow forms drop the keys you can most easily rediscover
        // (movement, panes) rather than the ones you can't (the filters).
        Mode::Normal => {
            if area.width >= 132 {
                "j/k move · / search · s startup · L level · f ai · m status · S sort · \
                 t tech · 1-6 mark · x hide · X hide co · u undo · a add · c edit · n note · \
                 o open · r fetch · e score · ? help · q quit"
                    .to_string()
            } else if area.width >= 92 {
                "j/k move · / search · s startup · L level · t tech · m status · S sort · \
                 1-6 mark · x hide · u undo · a add · ? help · q quit"
                    .to_string()
            } else {
                "s startup · L level · t tech · m status · x hide · a add · ? help".to_string()
            }
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
        Line::from("           L   level: the filters.yaml policy, then each rung"),
        Line::from("               (intern/junior/mid/senior/staff/manager),"),
        Line::from("               then unlabelled, then any"),
        Line::from("           t   pick a technology (aws, gcp, rust, ...) — the list"),
        Line::from("               shows only tags present in your data, with counts"),
        Line::from("           T   clear the technology filter"),
        Line::from("           f   AI verdict: all/apply/consider/skip/unscored"),
        Line::from("           m   your status: all/tracked/stale/dismissed/"),
        Line::from("               untouched/applied/recruiter/interview/"),
        Line::from("               rejected/skipped/silence"),
        Line::from("           /   search company, title and location"),
        Line::from("           S   sort: score/company/title/stage/posted/"),
        Line::from("               status/level/age/odds/qualified"),
        Line::from("               (status sorts interview first, untouched last;"),
        Line::from("                age sorts longest-untouched first)"),
        Line::from("           R   reverse sort"),
        Line::from(""),
        Line::from("  TRIAGE   in pipeline order:"),
        Line::from("           1 applied   2 recruiter  3 interview"),
        Line::from("           4 rejected  5 skipped    6 silence   0 clear"),
        Line::from("           n   edit notes          o  open in browser"),
        Line::from(""),
        Line::from("  HIDE     x   hide this posting (stays hidden across refreshes)"),
        Line::from("           X   hide EVERY posting from this employer, now and"),
        Line::from("               in future — the one for staffing agencies"),
        Line::from("           u   restore whatever is hiding the selected row"),
        Line::from("               (m cycles to the 'dismissed' view to find them)"),
        Line::from(""),
        Line::from("  ENTRIES  a   add an application the scrapers never found"),
        Line::from("           c   edit the selected row"),
        Line::from("           D   delete it (hand-entered rows only)"),
        Line::from(""),
        Line::from("  RUN      r   fetch new jobs from every configured source"),
        Line::from("           e   score JUST the selected row with Claude"),
        Line::from("           E   score every unscored row in view (asks first)"),
        Line::from("           F5  reload from the database"),
        Line::from(""),
        Line::from(""),
        Line::from(Span::styled(
            "  The mine column shows how long a row has sat in its current",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(Span::styled(
            "  status; red means past the filters.yaml staleness threshold.",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(Span::styled(
            "  Startup mode is a view, not a fetch setting — everything is",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(Span::styled(
            "  always stored, so toggling it never needs a re-fetch.",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(Span::styled(
            "  Channel: A agency · R referral · I inbound · D direct.",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(Span::styled(
            "  Marks: + yours · - hidden · = both. Hiding never deletes;",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(Span::styled(
            "  the row keeps its score and notes, and u puts it back.",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(Span::styled(
            "  Rows marked + are yours. They are never hidden by startup",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(Span::styled(
            "  mode or demoted by refilter, and they score like any other.",
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

// ----------------------------------------------------------------- the form

/// Draw the manual-entry page: a label/value row per single-line field,
/// then the description box taking whatever height is left.
fn draw_form(f: &mut Frame, area: Rect, app: &App) {
    use super::form::{FieldKind, FormMode, FIELDS};

    let Some(form) = &app.form else { return };

    let outer = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Magenta))
        .title(form.title());
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    let single_line = FIELDS
        .iter()
        .filter(|s| s.kind != FieldKind::Multiline)
        .count() as u16;

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(single_line), // the label/value rows
            Constraint::Length(1),           // spacer
            Constraint::Min(4),              // description
            Constraint::Length(1),           // error / hint
            Constraint::Length(1),           // key bar
        ])
        .split(inner);

    // --- single-line fields -------------------------------------------
    let mut lines: Vec<Line> = Vec::new();
    for (i, spec) in FIELDS.iter().enumerate() {
        if spec.kind == FieldKind::Multiline {
            continue;
        }
        let focused = i == form.focus;
        let value = form.value_at(i);
        let mut spans = vec![Span::styled(
            format!("  {:<12}", spec.label),
            if focused {
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            },
        )];

        if spec.kind == FieldKind::Choice {
            // Arrows only on the focused row, so the page doesn't read as
            // a wall of chevrons.
            spans.push(Span::styled(
                if focused {
                    format!("‹ {value} ›")
                } else {
                    format!("  {value}  ")
                },
                Style::default().fg(Color::Cyan),
            ));
        } else {
            spans.push(Span::raw(value.to_string()));
            if focused && form.mode == FormMode::Insert {
                spans.push(Span::styled("█", Style::default().fg(Color::Magenta)));
            }
        }

        // The hint sits on the focused row only — it's guidance for what
        // you're typing now, not a permanent annotation.
        if focused && !spec.hint.is_empty() {
            spans.push(Span::styled(
                format!("   {}", spec.hint),
                Style::default().fg(Color::DarkGray),
            ));
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), rows[0]);

    // --- description ---------------------------------------------------
    let desc_index = FIELDS
        .iter()
        .position(|s| s.kind == FieldKind::Multiline)
        .expect("the form has a description field");
    let desc_focused = form.focus == desc_index;
    let desc = form.value_at(desc_index);

    let desc_block = Block::default()
        .borders(Borders::ALL)
        .border_style(if desc_focused {
            Style::default().fg(Color::Magenta)
        } else {
            Style::default().fg(Color::DarkGray)
        })
        .title(if desc_focused {
            " Description — paste the JD here ".to_string()
        } else {
            format!(" Description ({} chars) ", desc.chars().count())
        });

    let body = if desc.is_empty() && !desc_focused {
        Paragraph::new(Span::styled(
            "empty — without a description the AI step has only the title and company to judge from",
            Style::default().fg(Color::DarkGray),
        ))
    } else {
        let mut text = desc.to_string();
        if desc_focused && form.mode == FormMode::Insert {
            text.push('█');
        }
        Paragraph::new(text)
    };
    f.render_widget(
        body.block(desc_block)
            .wrap(Wrap { trim: false })
            .scroll((form.desc_scroll, 0)),
        rows[2],
    );

    // --- error / footer -------------------------------------------------
    let footer = match &form.error {
        Some(e) => Line::from(Span::styled(
            format!("  {e}"),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )),
        None => Line::from(Span::styled(
            "  Company and Title are required — everything else is optional.",
            Style::default().fg(Color::DarkGray),
        )),
    };
    let footer = match form.mode {
        FormMode::Command => Line::from(""),
        _ => footer,
    };
    f.render_widget(Paragraph::new(footer), rows[3]);

    // The key bar lives here rather than in `draw_keys`, because the form
    // is drawn as a whole page and returns before the shared footer is
    // ever laid out. Leaving it to that footer is exactly how the save
    // binding ended up invisible.
    let bar = match form.mode {
        FormMode::Command => Line::from(vec![
            Span::styled(
                format!("  :{}", form.command),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled("█", Style::default().fg(Color::Magenta)),
            Span::styled(
                "      :w write · :wq write and close · :q close · :q! discard",
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        FormMode::Insert => Line::from(vec![
            Span::styled(
                "  -- INSERT --",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("   esc", Style::default().fg(Color::Cyan)),
            Span::styled(" normal · ", Style::default().fg(Color::DarkGray)),
            Span::styled("tab", Style::default().fg(Color::Cyan)),
            Span::styled(" next field · ", Style::default().fg(Color::DarkGray)),
            Span::styled("enter", Style::default().fg(Color::Cyan)),
            Span::styled(
                if form.focused().kind == FieldKind::Multiline {
                    " newline"
                } else {
                    " next field"
                },
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        FormMode::Normal => Line::from(vec![
            Span::styled(
                "  -- NORMAL --",
                Style::default()
                    .fg(Color::Blue)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("   j/k", Style::default().fg(Color::Cyan)),
            Span::styled(" field · ", Style::default().fg(Color::DarkGray)),
            Span::styled("i", Style::default().fg(Color::Cyan)),
            Span::styled(" insert · ", Style::default().fg(Color::DarkGray)),
            Span::styled("h/l", Style::default().fg(Color::Cyan)),
            Span::styled(" cycle · ", Style::default().fg(Color::DarkGray)),
            Span::styled("dd", Style::default().fg(Color::Cyan)),
            Span::styled(" clear · ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "ZZ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" or ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                ":w",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" WRITE · ", Style::default().fg(Color::DarkGray)),
            Span::styled("ZQ", Style::default().fg(Color::Cyan)),
            Span::styled(" discard", Style::default().fg(Color::DarkGray)),
        ]),
    };
    f.render_widget(Paragraph::new(bar), rows[4]);
}

fn draw_confirm_delete(f: &mut Frame, area: Rect, app: &App) {
    let Some(row) = app.selected_row() else {
        return;
    };
    let popup = centered_rect(56, 22, area);
    f.render_widget(Clear, popup);
    f.render_widget(
        Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                format!("  Delete {} — {}?", row.company, row.title),
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "  This removes the entry, its score and your notes.",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(""),
            Line::from("  y to delete · any other key to cancel"),
        ])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Red))
                .title(" confirm "),
        ),
        popup,
    );
}

/// The dismiss confirmation. Spells out the blast radius, because `X`
/// hides an employer's entire future output and that is not obvious from
/// a keystroke.
fn draw_confirm_dismiss(f: &mut Frame, area: Rect, app: &App) {
    let Some(row) = app.selected_row() else {
        return;
    };
    let company_scope = app.mode == Mode::ConfirmDismissCompany;

    let mut lines = vec![Line::from("")];
    if company_scope {
        lines.push(Line::from(vec![
            Span::raw("  Hide every posting from "),
            Span::styled(
                row.company.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw("?"),
        ]));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  Including ones not fetched yet — this is the one for staffing",
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(Span::styled(
            "  agencies that repost the same roles under new URLs.",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        lines.push(Line::from(vec![
            Span::raw("  Hide "),
            Span::styled(
                format!("{} — {}", row.company, row.title),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw("?"),
        ]));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  It stays hidden across refreshes, including if it's reposted",
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(Span::styled(
            "  under a new URL.",
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Nothing is deleted. m cycles to the dismissed view, u restores.",
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from("  y to hide · any other key to cancel"));

    let popup = centered_rect(64, 34, area);
    f.render_widget(Clear, popup);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow))
                .title(if company_scope {
                    " dismiss employer "
                } else {
                    " dismiss posting "
                }),
        ),
        popup,
    );
}

/// The technology picker: the tags actually present in your data, with
/// counts, highest first.
///
/// Counts are the feature, not decoration — "aws 412 / gcp 88" answers the
/// question before you select anything.
fn draw_tech_picker(f: &mut Frame, area: Rect, app: &App) {
    let visible = app.tech_visible();
    let popup = centered_rect(52, 72, area);
    f.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green))
        .title(if app.tech_query.is_empty() {
            " technology ".to_string()
        } else {
            format!(" technology · {} ", app.tech_query)
        });
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    if visible.is_empty() {
        f.render_widget(
            Paragraph::new(if app.tech_choices.is_empty() {
                "No technologies detected.\n\nEither nothing is stored yet, or tech.yaml is \
                 missing or failed to load."
            } else {
                "Nothing matches what you've typed."
            })
            .style(Style::default().fg(Color::DarkGray))
            .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(inner);

    let widest = visible
        .iter()
        .map(|(t, _)| t.qualified().len())
        .max()
        .unwrap_or(20);

    let items: Vec<Line> = visible
        .iter()
        .enumerate()
        .map(|(i, (tag, n))| {
            let selected = i == app.tech_cursor;
            let active = app.tech_filter.as_ref() == Some(tag);
            Line::from(vec![
                Span::styled(
                    if selected { " > " } else { "   " },
                    Style::default().fg(Color::Green),
                ),
                Span::styled(
                    format!("{:<width$}", tag.qualified(), width = widest + 2),
                    if selected {
                        Style::default().add_modifier(Modifier::BOLD)
                    } else if active {
                        Style::default().fg(Color::Green)
                    } else {
                        Style::default()
                    },
                ),
                Span::styled(format!("{n:>5}"), Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect();

    // Scroll so the cursor stays on screen in a list of ~45 tags.
    let height = rows[0].height as usize;
    let offset = app.tech_cursor.saturating_sub(height.saturating_sub(2));
    f.render_widget(Paragraph::new(items).scroll((offset as u16, 0)), rows[0]);
    f.render_widget(
        Paragraph::new(Span::styled(
            format!(
                "  {} tags · enter select · 0 clear · esc cancel",
                visible.len()
            ),
            Style::default().fg(Color::DarkGray),
        )),
        rows[1],
    );
}

/// The batch-scoring confirmation.
///
/// States the count, because that's what the bill is proportional to and
/// it's invisible from the list when filters are on.
fn draw_confirm_evaluate(f: &mut Frame, area: Rect, app: &App) {
    let n = app.unscored_in_scope();
    let popup = centered_rect(58, 30, area);
    f.render_widget(Clear, popup);
    f.render_widget(
        Paragraph::new(vec![
            Line::from(""),
            Line::from(vec![
                Span::raw("  Score "),
                Span::styled(
                    format!("{n}"),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" unscored postings with Claude?"),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                "  This is the only thing here that costs money. The current",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                "  startup and level filters decide what's in the queue.",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "  `e` scores just the selected row instead.",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(""),
            Line::from("  y to score · any other key to cancel"),
        ])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow))
                .title(" score all "),
        ),
        popup,
    );
}
