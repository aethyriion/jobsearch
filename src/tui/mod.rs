//! The terminal event loop.
//!
//! Three things feed it, and `tokio::select!` merges them:
//!
//! 1. keyboard input, read on a dedicated OS thread (crossterm's read is
//!    blocking, and a blocking read on a tokio worker would starve the
//!    runtime the background fetches run on);
//! 2. progress from a background fetch or scoring run;
//! 3. a redraw tick, so a long-running task's progress keeps animating
//!    even when nothing else happens.
//!
//! Background work gets its OWN `Store`, not a shared one. SQLite in WAL
//! mode handles concurrent connections, and a second connection is far
//! simpler than making one `rusqlite::Connection` safely shareable across
//! tasks — the UI stays responsive and readable throughout a fetch.
//!
//! That background work runs on a dedicated OS thread with its own
//! current-thread runtime rather than via `tokio::spawn`. `Connection` is
//! `Send` but not `Sync`, so a future holding `&Store` across an `.await`
//! isn't `Send` and can't be spawned onto the shared pool. Pinning each
//! run to one thread sidesteps that without wrapping the connection in a
//! mutex that would serialize the UI's reads behind the fetch's writes.

pub mod app;
pub mod form;
pub mod ui;

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::{self, Stdout};
use std::time::Duration;
use tokio::sync::mpsc;

use crate::config::Paths;
use crate::filters::Filters;
use crate::pipeline::Progress;
use crate::store::Store;
use app::{App, Mode, Pane};

/// Anything that can wake the loop.
enum Tick {
    Input(Event),
    Task(TaskMessage),
    Redraw,
}

enum TaskMessage {
    Progress(Progress),
    Finished { label: String, summary: String },
    Failed { label: String, error: String },
}

pub async fn run(paths: Paths, filters: Filters) -> Result<()> {
    let store = Store::open(paths.db())?;
    let rows = store.all_candidates()?;
    let mut app = App::new(rows, filters.startup.clone());
    app.status = format!(
        "{} jobs loaded · {} with a known stage · ? for help",
        app.rows.len(),
        app.rows.iter().filter(|r| r.stage.is_some()).count()
    );

    let mut terminal = setup_terminal()?;
    let result = event_loop(&mut terminal, &mut app, &store, &filters, &paths).await;
    restore_terminal(&mut terminal)?;
    result
}

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
    store: &Store,
    filters: &Filters,
    paths: &Paths,
) -> Result<()> {
    let (input_tx, mut input_rx) = mpsc::unbounded_channel();
    let (task_tx, mut task_rx) = mpsc::unbounded_channel::<TaskMessage>();

    // crossterm's read() blocks; keep it off the async runtime entirely.
    // The loop ends on either a read error or a closed receiver (the UI
    // has quit), at which point this thread has nothing left to do.
    std::thread::spawn(move || {
        while let Ok(ev) = event::read() {
            if input_tx.send(ev).is_err() {
                break;
            }
        }
    });

    let mut redraw = tokio::time::interval(Duration::from_millis(120));
    redraw.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    terminal.draw(|f| ui::draw(f, app))?;

    loop {
        let tick = tokio::select! {
            // `recv()` yielding None means the input thread has gone —
            // `event::read()` failed, which is what happens when the
            // terminal itself goes away (closed window, dropped SSH
            // session). Exit on that directly. Matching only on `Some`
            // would disable this branch and leave the loop running on the
            // redraw tick; in practice the next `terminal.draw()` fails on
            // the dead terminal and propagates, so the process still ends
            // — but by I/O error rather than on purpose, and only because
            // drawing happens to come next.
            ev = input_rx.recv() => match ev {
                Some(ev) => Tick::Input(ev),
                None => return Ok(()),
            },
            Some(msg) = task_rx.recv() => Tick::Task(msg),
            _ = redraw.tick() => Tick::Redraw,
        };

        match tick {
            Tick::Input(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                handle_key(app, store, filters, paths, key, &task_tx)?;
            }
            // A bracketed paste arrives as one event rather than as a
            // burst of synthetic keystrokes — which is what makes pasting
            // a multi-KB job description instant, and stops a stray
            // character inside it being read as a command.
            Tick::Input(Event::Paste(text)) => match app.mode {
                Mode::Form => {
                    if let Some(form) = &mut app.form {
                        form.insert_str(&text);
                    }
                }
                Mode::Search | Mode::Notes => {
                    app.input.push_str(text.replace(['\n', '\r'], " ").trim());
                }
                _ => {}
            },
            Tick::Input(Event::Resize(_, _)) => {}
            Tick::Input(_) => continue,
            Tick::Task(msg) => apply_task_message(app, store, msg)?,
            Tick::Redraw => {}
        }

        if app.should_quit {
            return Ok(());
        }
        terminal.draw(|f| ui::draw(f, app))?;
    }
}

fn apply_task_message(app: &mut App, store: &Store, msg: TaskMessage) -> Result<()> {
    match msg {
        TaskMessage::Progress(p) => match p {
            Progress::SourceDone {
                name,
                fetched,
                kept,
            } => {
                app.task.done += 1;
                app.task.detail = format!("{name} — {fetched} fetched, {kept} new");
            }
            Progress::SourceFailed { name, error } => {
                app.task.done += 1;
                app.task.failures += 1;
                app.task.errors.push(format!("{name}: {error}"));
                app.task.detail = format!("{name} — failed");
            }
            Progress::Scored {
                index,
                total,
                company,
                title,
                score,
                recommendation,
            } => {
                app.task.done = index;
                app.task.total = total;
                app.task.detail = format!("{score:3} {recommendation:9} {company} — {title}");
            }
            Progress::ScoreFailed {
                company,
                title,
                error,
            } => {
                app.task.done += 1;
                app.task.failures += 1;
                app.task
                    .errors
                    .push(format!("{company} — {title}: {error}"));
            }
            Progress::Note(note) => app.task.detail = note,
        },
        TaskMessage::Finished { label, summary } => {
            app.task.running = false;
            // Reload rather than patch: a fetch can have added rows,
            // changed stages and scored jobs all at once, and re-reading
            // is both simpler and cheaper than reconciling that by hand.
            app.reload(store)?;
            app.status = format!("{label} finished — {summary}");
            if app.task.failures > 0 {
                app.status
                    .push_str(&format!(" ({} failed)", app.task.failures));
            }
        }
        TaskMessage::Failed { label, error } => {
            app.task.running = false;
            app.status = format!("{label} failed — {error}");
        }
    }
    Ok(())
}

fn handle_key(
    app: &mut App,
    store: &Store,
    filters: &Filters,
    paths: &Paths,
    key: KeyEvent,
    task_tx: &mpsc::UnboundedSender<TaskMessage>,
) -> Result<()> {
    // Text-entry modes swallow everything except their own controls, so a
    // company name containing "q" can't quit the app mid-search.
    match app.mode {
        Mode::Search => {
            match key.code {
                KeyCode::Esc => {
                    app.mode = Mode::Normal;
                    app.input.clear();
                }
                KeyCode::Enter => {
                    app.search = app.input.clone();
                    app.input.clear();
                    app.mode = Mode::Normal;
                    app.recompute();
                    app.status = if app.search.is_empty() {
                        "search cleared".into()
                    } else {
                        format!("{} match \"{}\"", app.view.len(), app.search)
                    };
                }
                KeyCode::Backspace => {
                    app.input.pop();
                }
                KeyCode::Char(c) => app.input.push(c),
                _ => {}
            }
            return Ok(());
        }
        Mode::Notes => {
            match key.code {
                KeyCode::Esc => {
                    app.mode = Mode::Normal;
                    app.input.clear();
                }
                KeyCode::Enter => {
                    app.save_notes(store)?;
                    app.input.clear();
                    app.mode = Mode::Normal;
                }
                KeyCode::Backspace => {
                    app.input.pop();
                }
                KeyCode::Char(c) => app.input.push(c),
                _ => {}
            }
            return Ok(());
        }
        Mode::Help => {
            app.mode = Mode::Normal;
            return Ok(());
        }
        Mode::Form => {
            handle_form_key(app, store, key)?;
            return Ok(());
        }
        Mode::ConfirmDelete => {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => app.delete_selected(store)?,
                _ => {
                    app.mode = Mode::Normal;
                    app.status = "not deleted".into();
                }
            }
            return Ok(());
        }
        Mode::Normal => {}
    }

    let scroll_step: i16 = 10;
    // Any key that isn't the second half of `dd` cancels the pending
    // sequence, so a stray `d` can't silently arm a later delete.
    if !matches!(key.code, KeyCode::Char('d')) {
        app.pending_key = None;
    }
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.should_quit = true
        }
        KeyCode::Char('?') => app.mode = Mode::Help,

        KeyCode::Tab => {
            app.pane = if app.pane == Pane::List {
                Pane::Detail
            } else {
                Pane::List
            }
        }

        // Movement applies to whichever pane has focus, so the same keys
        // work without the user tracking which mode they're in.
        KeyCode::Char('j') | KeyCode::Down => match app.pane {
            Pane::List => app.move_selection(1),
            Pane::Detail => app.scroll_detail(1),
        },
        KeyCode::Char('k') | KeyCode::Up => match app.pane {
            Pane::List => app.move_selection(-1),
            Pane::Detail => app.scroll_detail(-1),
        },
        KeyCode::Char('J') | KeyCode::PageDown => app.scroll_detail(scroll_step),
        KeyCode::Char('K') | KeyCode::PageUp => app.scroll_detail(-scroll_step),
        KeyCode::Char('g') | KeyCode::Home => app.select_first(),
        KeyCode::Char('G') | KeyCode::End => app.select_last(),

        KeyCode::Char('s') => app.toggle_startup(),
        KeyCode::Char('f') => app.cycle_ai_filter(),
        KeyCode::Char('m') => app.cycle_my_filter(),
        KeyCode::Char('S') => app.cycle_sort(),
        KeyCode::Char('R') => app.toggle_sort_dir(),

        KeyCode::Char('/') => {
            app.mode = Mode::Search;
            app.input = app.search.clone();
        }
        KeyCode::Char('n') => {
            app.mode = Mode::Notes;
            app.input = app
                .selected_row()
                .and_then(|r| r.notes.clone())
                .unwrap_or_default();
        }
        KeyCode::Char('o') | KeyCode::Enter => app.open_selected(),

        KeyCode::Char('0') => app.set_status(store, None)?,
        KeyCode::Char(c @ '1'..='5') => {
            let index = c.to_digit(10).unwrap() as usize - 1;
            if let Some(status) = App::status_key(index) {
                app.set_status(store, Some(status))?;
            }
        }

        KeyCode::Char('a') | KeyCode::Char('i') => app.open_add_form(),
        KeyCode::Char('c') | KeyCode::Char('E') => app.open_edit_form(),
        // `dd` to delete, the way it deletes a line everywhere else. `D`
        // stays as a single-key alias.
        KeyCode::Char('d') => {
            if app.pending_key == Some('d') {
                app.pending_key = None;
                app.request_delete();
            } else {
                app.pending_key = Some('d');
            }
        }
        KeyCode::Char('D') => app.request_delete(),

        KeyCode::F(5) => {
            app.reload(store)?;
            app.status = format!("reloaded — {} jobs", app.rows.len());
        }

        KeyCode::Char('r') => spawn_fetch(app, filters, paths, task_tx),
        KeyCode::Char('e') => spawn_evaluate(app, filters, paths, task_tx),

        _ => {}
    }
    Ok(())
}

/// Guard against starting a second background run while one is going.
/// Both tasks write to the same database and the same progress slot, so
/// two at once would interleave into nonsense.
fn begin_task(app: &mut App, label: &str) -> bool {
    if app.task.running {
        app.status = format!("{} already running", app.task.label);
        return false;
    }
    app.task = app::TaskState {
        running: true,
        label: label.to_string(),
        detail: "starting…".into(),
        ..Default::default()
    };
    true
}

fn spawn_fetch(
    app: &mut App,
    filters: &Filters,
    paths: &Paths,
    task_tx: &mpsc::UnboundedSender<TaskMessage>,
) {
    if !begin_task(app, "fetch") {
        return;
    }
    let paths = paths.clone();
    let tx = task_tx.clone();
    // The compiled filters can't cross a task boundary (Regex is Sync but
    // Filters is borrowed from the caller), so the background task
    // re-reads and re-compiles filters.yaml. That's a few milliseconds,
    // and it has a real upside: a fetch started after you edited the file
    // uses what's on disk now.
    let _ = filters;

    spawn_blocking_run(move || async move {
        let result = async {
            let store = Store::open(paths.db())?;
            let filters = Filters::compile(&crate::config::load_filters(paths.filters())?)?;
            let companies = crate::config::load_companies(paths.companies())?;
            let aggs = crate::config::load_aggregators(paths.aggregators())?;
            let tx2 = tx.clone();
            let summary = crate::pipeline::run_fetch(
                &store,
                &filters,
                &companies,
                &aggs,
                crate::pipeline::DEFAULT_CONCURRENCY,
                |p| {
                    let _ = tx2.send(TaskMessage::Progress(p));
                },
            )
            .await?;

            let discovered = crate::discover::append_new_companies(
                paths.companies(),
                &summary.discovered,
                "aggregator full-JD fetch",
            )?;
            crate::pipeline::write_candidates_csv(&store, paths.candidates_csv())?;

            Ok::<_, anyhow::Error>(format!(
                "{} fetched, {} new candidates, {} companies added",
                summary.fetched,
                summary.new_candidates,
                discovered.len()
            ))
        }
        .await;

        let _ = match result {
            Ok(summary) => tx.send(TaskMessage::Finished {
                label: "fetch".into(),
                summary,
            }),
            Err(e) => tx.send(TaskMessage::Failed {
                label: "fetch".into(),
                error: format!("{e:#}"),
            }),
        };
    });
}

fn spawn_evaluate(
    app: &mut App,
    filters: &Filters,
    paths: &Paths,
    task_tx: &mpsc::UnboundedSender<TaskMessage>,
) {
    if !begin_task(app, "evaluate") {
        return;
    }
    let paths = paths.clone();
    let tx = task_tx.clone();
    // Scoring is the only operation here that spends money, so it follows
    // the startup toggle as the user currently has it rather than the
    // configured default — what you see is what you pay to score.
    let startup_only = app.startup_only;
    let _ = filters;

    spawn_blocking_run(move || async move {
        let result = async {
            let store = Store::open(paths.db())?;
            let filters = Filters::compile(&crate::config::load_filters(paths.filters())?)?;
            let profile = crate::config::load_profile(paths.profile())?;
            let evaluator =
                crate::evaluate::Evaluator::new(&profile, crate::evaluate::model_from_env())?;
            let tx2 = tx.clone();
            let scored = crate::pipeline::run_evaluate(
                &store,
                &filters,
                &evaluator,
                startup_only,
                None,
                |p| {
                    let _ = tx2.send(TaskMessage::Progress(p));
                },
            )
            .await?;
            crate::pipeline::write_scored_csv(&store, paths.scored_csv())?;
            Ok::<_, anyhow::Error>(format!("{scored} scored"))
        }
        .await;

        let _ = match result {
            Ok(summary) => tx.send(TaskMessage::Finished {
                label: "evaluate".into(),
                summary,
            }),
            Err(e) => tx.send(TaskMessage::Failed {
                label: "evaluate".into(),
                error: format!("{e:#}"),
            }),
        };
    });
}

/// Keys inside the manual-entry form.
///
/// Modal, like the rest of this UI's vocabulary: Insert to type, Esc to
/// Normal, `ZZ` or `:w` to write. Deliberately no Ctrl+S — a save key you
/// have to be told about is a save key nobody finds, and `:w` is one this
/// audience already knows.
fn handle_form_key(app: &mut App, store: &Store, key: KeyEvent) -> Result<()> {
    use crate::tui::form::{FieldKind, FormAction, FormMode};

    let Some(form) = &mut app.form else {
        return Ok(());
    };

    let action = match form.mode {
        // ---------------------------------------------------- command line
        FormMode::Command => match key.code {
            KeyCode::Esc => {
                form.enter_normal();
                FormAction::None
            }
            KeyCode::Enter => form.command_submit(),
            KeyCode::Backspace => form.command_backspace(),
            KeyCode::Char(c) => {
                form.command_push(c);
                FormAction::None
            }
            _ => FormAction::None,
        },

        // --------------------------------------------------------- normal
        FormMode::Normal => match key.code {
            KeyCode::Char(c) => form.normal_key(c),
            KeyCode::Tab | KeyCode::Down => {
                form.next_field();
                FormAction::None
            }
            KeyCode::BackTab | KeyCode::Up => {
                form.prev_field();
                FormAction::None
            }
            KeyCode::Left => {
                form.cycle(-1);
                FormAction::None
            }
            KeyCode::Right => {
                form.cycle(1);
                FormAction::None
            }
            KeyCode::Enter => {
                form.enter_insert();
                FormAction::None
            }
            // Esc in Normal is a no-op rather than a cancel: losing a
            // half-typed application to one stray keypress would be a bad
            // trade for saving a `:q!`.
            _ => FormAction::None,
        },

        // --------------------------------------------------------- insert
        FormMode::Insert => {
            let kind = form.focused().kind;
            match key.code {
                KeyCode::Esc => {
                    form.enter_normal();
                    FormAction::None
                }
                KeyCode::Tab | KeyCode::Down => {
                    form.next_field();
                    FormAction::None
                }
                KeyCode::BackTab | KeyCode::Up => {
                    form.prev_field();
                    FormAction::None
                }
                KeyCode::Enter => {
                    form.newline();
                    FormAction::None
                }
                KeyCode::Backspace => {
                    form.backspace();
                    FormAction::None
                }
                KeyCode::Left if kind == FieldKind::Choice => {
                    form.cycle(-1);
                    FormAction::None
                }
                KeyCode::Right if kind == FieldKind::Choice => {
                    form.cycle(1);
                    FormAction::None
                }
                KeyCode::PageDown if kind == FieldKind::Multiline => {
                    form.scroll_description(5);
                    FormAction::None
                }
                KeyCode::PageUp if kind == FieldKind::Multiline => {
                    form.scroll_description(-5);
                    FormAction::None
                }
                KeyCode::Char(' ') if kind == FieldKind::Choice => {
                    form.cycle(1);
                    FormAction::None
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    form.insert_char(c);
                    FormAction::None
                }
                _ => FormAction::None,
            }
        }
    };

    match action {
        FormAction::None => {}
        FormAction::Save => {
            app.save_form(store)?;
        }
        FormAction::Discard => app.cancel_form(),
        FormAction::QuitIfClean => {
            let dirty = app.form.as_ref().is_some_and(|f| f.dirty);
            if dirty {
                if let Some(form) = &mut app.form {
                    form.error = Some(
                        "No write since last change (add ! to override, or :w to save)".into(),
                    );
                }
            } else {
                app.cancel_form();
            }
        }
    }
    Ok(())
}

/// Run one background operation on a dedicated thread with its own
/// current-thread runtime.
///
/// `tokio::spawn` can't take these: they hold a `&Store` across `.await`,
/// and `rusqlite::Connection` is `Send` but not `Sync`, so the future
/// isn't `Send`. A thread per run is the honest shape anyway — there is
/// at most one in flight (see `begin_task`), and it wants a whole SQLite
/// connection to itself regardless.
fn spawn_blocking_run<F, Fut>(make_future: F)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()>,
{
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            // Nothing useful to do if a runtime won't start; the UI keeps
            // running and the task simply never reports back, which
            // `begin_task` will surface as a stuck "running" label.
            Err(_) => return,
        };
        runtime.block_on(make_future());
    });
}

// ---------------------------------------------------------------- terminal

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode().context("entering raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)
        .context("entering the alternate screen")?;
    let terminal = Terminal::new(CrosstermBackend::new(stdout)).context("creating the terminal")?;
    // A panic in raw mode leaves the terminal unusable — no echo, no line
    // discipline, in the alternate screen — and the user has to blind-type
    // `reset`. Restore first, then let the panic print normally.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), DisableBracketedPaste, LeaveAlternateScreen);
        default_hook(info);
    }));
    Ok(terminal)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableBracketedPaste,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    Ok(())
}
