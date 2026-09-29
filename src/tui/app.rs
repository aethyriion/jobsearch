//! TUI state and the transitions over it.
//!
//! Kept free of rendering and of terminal I/O on purpose: everything here
//! is a plain data transformation, so the filter/sort/selection rules are
//! testable without a terminal. `ui.rs` draws this state; `mod.rs` owns
//! the event loop and the background tasks.

use crate::filters::{SeniorityPolicy, StartupPolicy};
use crate::model::{JobRow, Seniority, MY_STATUS_VALUES};
use crate::store::Store;
use anyhow::Result;
use ratatui::widgets::TableState;

/// What the user is currently typing into, if anything. Normal mode is
/// where every single-key binding lives; the others capture text and are
/// deliberately exclusive so a keystroke can never mean two things.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Search,
    Notes,
    Help,
    /// The manual-entry page, adding or editing an application by hand.
    Form,
    /// Confirming deletion of the selected manual entry.
    ConfirmDelete,
    /// Confirming dismissal of the selected posting.
    ConfirmDismissJob,
    /// Confirming dismissal of every posting from the selected employer.
    ConfirmDismissCompany,
    /// Confirming a batch scoring run, which costs real money.
    ConfirmEvaluateAll,
    /// Picking a technology to filter by.
    TechPicker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    List,
    Detail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Score,
    Company,
    Title,
    Stage,
    Posted,
    /// Your own tracking state, ordered by where it sits in the pipeline
    /// rather than alphabetically — see `JobRow::status_rank`.
    Status,
    /// Role seniority, lowest rung first.
    Level,
    /// How long the current status has been sitting. Untracked rows have
    /// no age and sort last either way.
    Age,
}

impl SortKey {
    pub fn label(self) -> &'static str {
        match self {
            SortKey::Score => "score",
            SortKey::Company => "company",
            SortKey::Title => "title",
            SortKey::Stage => "stage",
            SortKey::Posted => "posted",
            SortKey::Status => "status",
            SortKey::Level => "level",
            SortKey::Age => "age",
        }
    }

    pub fn next(self) -> Self {
        match self {
            SortKey::Score => SortKey::Company,
            SortKey::Company => SortKey::Title,
            SortKey::Title => SortKey::Stage,
            SortKey::Stage => SortKey::Posted,
            SortKey::Posted => SortKey::Status,
            SortKey::Status => SortKey::Level,
            SortKey::Level => SortKey::Age,
            SortKey::Age => SortKey::Score,
        }
    }
}

/// The AI's verdict, as a filter. `Unscored` is a first-class value rather
/// than "missing": "what hasn't been scored yet" is one of the most useful
/// views in the app, since it's the queue the next paid run will work on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiFilter {
    All,
    Apply,
    Consider,
    Skip,
    Unscored,
}

impl AiFilter {
    pub fn label(self) -> &'static str {
        match self {
            AiFilter::All => "all",
            AiFilter::Apply => "apply",
            AiFilter::Consider => "consider",
            AiFilter::Skip => "skip",
            AiFilter::Unscored => "unscored",
        }
    }

    pub fn next(self) -> Self {
        match self {
            AiFilter::All => AiFilter::Apply,
            AiFilter::Apply => AiFilter::Consider,
            AiFilter::Consider => AiFilter::Skip,
            AiFilter::Skip => AiFilter::Unscored,
            AiFilter::Unscored => AiFilter::All,
        }
    }

    fn matches(self, row: &JobRow) -> bool {
        match self {
            AiFilter::All => true,
            AiFilter::Apply => row.ai_status() == "apply",
            AiFilter::Consider => row.ai_status() == "consider",
            AiFilter::Skip => row.ai_status() == "skip",
            AiFilter::Unscored => row.recommendation.is_none(),
        }
    }
}

/// Your own tracking, as a filter. `Untouched` is the default working set
/// — jobs you haven't yet decided anything about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MyFilter {
    All,
    /// Anything you've acted on at all — the application tracker view.
    Tracked,
    /// Tracked rows that have sat in the same status past its configured
    /// threshold. The follow-up queue.
    Stale,
    /// The rows you've hidden. The only view that shows them, and where
    /// `u` puts one back.
    Dismissed,
    Untouched,
    Applied,
    Recruiter,
    Interview,
    Rejected,
    Skipped,
    Silence,
}

impl MyFilter {
    pub fn label(self) -> &'static str {
        match self {
            MyFilter::All => "all",
            MyFilter::Tracked => "tracked",
            MyFilter::Stale => "stale",
            MyFilter::Dismissed => "dismissed",
            MyFilter::Untouched => "untouched",
            MyFilter::Applied => "applied",
            MyFilter::Recruiter => "recruiter",
            MyFilter::Interview => "interview",
            MyFilter::Rejected => "rejected",
            MyFilter::Skipped => "skipped",
            MyFilter::Silence => "silence",
        }
    }

    pub fn next(self) -> Self {
        match self {
            MyFilter::All => MyFilter::Tracked,
            // Dismissed sits next to Tracked rather than at the end of the
            // chain: both are "rows I've made a decision about", and
            // burying the only way to find a hidden row nine keypresses
            // deep would make it effectively undiscoverable.
            // Stale sits directly after Tracked because it's a subset of
            // it: "everything I'm tracking", then "the part that's gone
            // quiet".
            MyFilter::Tracked => MyFilter::Stale,
            MyFilter::Stale => MyFilter::Dismissed,
            MyFilter::Dismissed => MyFilter::Untouched,
            MyFilter::Untouched => MyFilter::Applied,
            MyFilter::Applied => MyFilter::Recruiter,
            MyFilter::Recruiter => MyFilter::Interview,
            MyFilter::Interview => MyFilter::Rejected,
            MyFilter::Rejected => MyFilter::Skipped,
            MyFilter::Skipped => MyFilter::Silence,
            MyFilter::Silence => MyFilter::All,
        }
    }

    /// `stale` is passed in rather than derived here: staleness depends on
    /// the configured per-status thresholds, which a plain enum can't see.
    fn matches(self, row: &JobRow, stale: bool) -> bool {
        // Dismissed rows are invisible to every other filter value. That's
        // the whole point — they only appear when you ask for them, and
        // `Dismissed` is the only value that asks.
        if self == MyFilter::Dismissed {
            return row.dismissed;
        }
        if row.dismissed {
            return false;
        }
        match self {
            MyFilter::All => true,
            MyFilter::Tracked => row.my_status.is_some(),
            MyFilter::Stale => stale,
            MyFilter::Untouched => row.my_status.is_none(),
            other => row.my_status.as_deref() == Some(other.label()),
        }
    }
}

/// The level filter, as cycled by `L`. `Unlabelled` is a first-class
/// value because a bare "Software Engineer" carries no level and those are
/// common enough to want to look at on their own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LevelFilter {
    /// Everything the configured policy admits.
    Policy,
    Only(Seniority),
    Unlabelled,
    /// Every level, ignoring the policy entirely.
    All,
}

impl LevelFilter {
    pub fn label(self) -> String {
        match self {
            LevelFilter::Policy => "policy".to_string(),
            LevelFilter::Only(l) => l.as_str().to_string(),
            LevelFilter::Unlabelled => "unlabelled".to_string(),
            LevelFilter::All => "any".to_string(),
        }
    }

    pub fn next(self) -> Self {
        match self {
            LevelFilter::Policy => LevelFilter::Only(Seniority::Intern),
            LevelFilter::Only(Seniority::Manager) => LevelFilter::Unlabelled,
            LevelFilter::Only(l) => {
                let i = Seniority::ALL.iter().position(|x| *x == l).unwrap_or(0);
                LevelFilter::Only(Seniority::ALL[(i + 1) % Seniority::ALL.len()])
            }
            LevelFilter::Unlabelled => LevelFilter::All,
            LevelFilter::All => LevelFilter::Policy,
        }
    }

    fn matches(self, row: &JobRow, policy: &SeniorityPolicy) -> bool {
        match self {
            LevelFilter::Policy => policy.admits(row),
            LevelFilter::All => true,
            LevelFilter::Unlabelled => row.seniority().is_none(),
            LevelFilter::Only(want) => row.seniority() == Some(want),
        }
    }
}

/// A long-running background operation, for the status bar.
#[derive(Debug, Clone, Default)]
pub struct TaskState {
    pub running: bool,
    pub label: String,
    pub detail: String,
    pub done: usize,
    pub total: usize,
    /// Failures are counted rather than shown one by one — a run over 130
    /// boards normally has a few, and a scrolling error feed would bury
    /// the progress it's meant to accompany. The count is the signal;
    /// `errors` keeps the text for the log view.
    pub failures: usize,
    pub errors: Vec<String>,
}

pub struct App {
    pub rows: Vec<JobRow>,
    /// Indices into `rows`, after filtering and sorting. Keeping the view
    /// as indices rather than cloned rows means a status edit mutates one
    /// place and is visible everywhere at once.
    pub view: Vec<usize>,
    pub selected: usize,

    pub mode: Mode,
    pub pane: Pane,
    pub sort: SortKey,
    pub sort_desc: bool,

    pub startup_only: bool,
    pub startup_policy: StartupPolicy,
    pub seniority_policy: SeniorityPolicy,
    /// Days-in-status before a tracked row is flagged. Copied in rather
    /// than reached for through `Filters`, so rendering never needs the
    /// whole filter set.
    pub staleness: std::collections::BTreeMap<String, i64>,
    pub level_filter: LevelFilter,
    pub ai_filter: AiFilter,
    pub my_filter: MyFilter,
    pub search: String,
    /// Buffer for whichever text mode is active. Separate from `search` so
    /// starting a note doesn't wipe the active search.
    pub input: String,

    pub detail_scroll: u16,
    pub status: String,
    pub task: TaskState,
    pub should_quit: bool,

    /// Owned by the app rather than rebuilt per frame so the viewport
    /// offset persists. A fresh `TableState` every render would reset the
    /// offset to zero and let ratatui re-derive it, which technically
    /// keeps the selection on screen but pins it to whichever edge you
    /// approached from — the list appears to jump rather than scroll.
    pub table_state: TableState,

    /// First key of a pending two-key sequence in the list (`dd`).
    pub pending_key: Option<char>,

    /// The tech tag currently filtering the list, if any.
    pub tech_filter: Option<crate::tech::Tag>,
    /// Picker state: the tags present in the data with their counts,
    /// highest first, plus the cursor.
    pub tech_choices: Vec<(crate::tech::Tag, usize)>,
    pub tech_cursor: usize,
    /// Incremental filter typed into the picker.
    pub tech_query: String,

    /// Live only in `Mode::Form`. Held here rather than in the event loop
    /// so a redraw tick can render a half-filled form without it being
    /// threaded through every call.
    pub form: Option<super::form::Form>,
}

impl App {
    pub fn new(
        rows: Vec<JobRow>,
        startup_policy: StartupPolicy,
        seniority_policy: SeniorityPolicy,
        staleness: std::collections::BTreeMap<String, i64>,
    ) -> Self {
        let mut app = Self {
            rows,
            view: Vec::new(),
            selected: 0,
            mode: Mode::Normal,
            pane: Pane::List,
            sort: SortKey::Score,
            sort_desc: true,
            startup_only: startup_policy.enabled_by_default,
            startup_policy,
            seniority_policy,
            staleness,
            level_filter: LevelFilter::Policy,
            ai_filter: AiFilter::All,
            my_filter: MyFilter::All,
            search: String::new(),
            input: String::new(),
            detail_scroll: 0,
            status: String::new(),
            task: TaskState::default(),
            should_quit: false,
            table_state: TableState::default(),
            pending_key: None,
            tech_filter: None,
            tech_choices: Vec::new(),
            tech_cursor: 0,
            tech_query: String::new(),
            form: None,
        };
        app.recompute();
        app
    }

    /// Keep the widget's selection in step with ours. Called once per
    /// draw; the offset inside `table_state` is left alone so ratatui can
    /// scroll it minimally.
    pub fn sync_table_state(&mut self) {
        if self.view.is_empty() {
            self.table_state.select(None);
        } else {
            self.table_state
                .select(Some(self.selected.min(self.view.len() - 1)));
        }
    }

    pub fn selected_row(&self) -> Option<&JobRow> {
        self.view.get(self.selected).map(|&i| &self.rows[i])
    }

    /// Rebuild `view` from the current filters and sort.
    ///
    /// Selection is preserved *by URL*, not by index. That matters every
    /// time the view changes under the cursor — toggling startup mode,
    /// marking a job in a filtered view so it drops out, a background
    /// fetch adding rows — where keeping the index would silently move the
    /// cursor to an unrelated job and make the next keystroke act on the
    /// wrong one.
    pub fn recompute(&mut self) {
        let anchor = self.selected_row().map(|r| r.url.clone());
        let needle = self.search.to_lowercase();

        let mut view: Vec<usize> = (0..self.rows.len())
            .filter(|&i| {
                let row = &self.rows[i];
                if self.startup_only && !self.startup_policy.admits(row) {
                    return false;
                }
                if !self.level_filter.matches(row, &self.seniority_policy) {
                    return false;
                }
                if let Some(tag) = &self.tech_filter {
                    if !row.has_tech(tag) {
                        return false;
                    }
                }
                if !self.ai_filter.matches(row) {
                    return false;
                }
                if !self.my_filter.matches(row, self.is_stale(row)) {
                    return false;
                }
                if !needle.is_empty() {
                    let hay =
                        format!("{} {} {}", row.company, row.title, row.location).to_lowercase();
                    if !hay.contains(&needle) {
                        return false;
                    }
                }
                true
            })
            .collect();

        let rows = &self.rows;
        let sort = self.sort;
        view.sort_by(|&a, &b| {
            let (ra, rb) = (&rows[a], &rows[b]);
            match sort {
                // Unscored jobs sort as -1 rather than 0, so they land
                // below a genuine zero score instead of mixing with it.
                SortKey::Score => ra
                    .match_score
                    .unwrap_or(-1)
                    .cmp(&rb.match_score.unwrap_or(-1)),
                SortKey::Company => ra.company.to_lowercase().cmp(&rb.company.to_lowercase()),
                SortKey::Title => ra.title.to_lowercase().cmp(&rb.title.to_lowercase()),
                // Unknown stage sorts last in either direction — it's an
                // absence, and burying it beats having it head the list.
                SortKey::Stage => ra
                    .stage
                    .map(|s| s as u8)
                    .unwrap_or(u8::MAX)
                    .cmp(&rb.stage.map(|s| s as u8).unwrap_or(u8::MAX)),
                SortKey::Posted => ra.posted_at.cmp(&rb.posted_at),
                // Unlabelled last in either direction: it's an absence,
                // and burying it beats having it head the list.
                // Oldest first when descending, which is what you want:
                // the thing that's been sitting longest is the one to
                // chase. Untracked rows sort last in both directions.
                SortKey::Age => ra
                    .status_age_days()
                    .unwrap_or(-1)
                    .cmp(&rb.status_age_days().unwrap_or(-1)),
                SortKey::Level => ra
                    .seniority()
                    .map(|l| l as u8)
                    .unwrap_or(u8::MAX)
                    .cmp(&rb.seniority().map(|l| l as u8).unwrap_or(u8::MAX)),
                SortKey::Status => {
                    status_rank(ra.my_status.as_deref()).cmp(&status_rank(rb.my_status.as_deref()))
                }
            }
            .then_with(|| ra.company.cmp(&rb.company))
        });
        if self.sort_desc {
            view.reverse();
        }

        self.view = view;
        self.selected = anchor
            .and_then(|url| self.view.iter().position(|&i| self.rows[i].url == url))
            .unwrap_or(0)
            .min(self.view.len().saturating_sub(1));
        self.detail_scroll = 0;
    }

    // ------------------------------------------------------- navigation

    pub fn move_selection(&mut self, delta: isize) {
        if self.view.is_empty() {
            return;
        }
        let len = self.view.len() as isize;
        let next = (self.selected as isize + delta).clamp(0, len - 1);
        if next != self.selected as isize {
            self.selected = next as usize;
            self.detail_scroll = 0;
        }
    }

    pub fn select_first(&mut self) {
        self.selected = 0;
        self.detail_scroll = 0;
    }

    pub fn select_last(&mut self) {
        self.selected = self.view.len().saturating_sub(1);
        self.detail_scroll = 0;
    }

    pub fn scroll_detail(&mut self, delta: i16) {
        self.detail_scroll = self
            .detail_scroll
            .saturating_add_signed(delta)
            .min(u16::MAX / 2);
    }

    // ----------------------------------------------------------- actions

    pub fn toggle_startup(&mut self) {
        self.startup_only = !self.startup_only;
        self.recompute();
        self.status = if self.startup_only {
            format!("startup mode ON ({})", self.startup_policy.describe())
        } else {
            "startup mode OFF — showing every stage".to_string()
        };
    }

    pub fn cycle_ai_filter(&mut self) {
        self.ai_filter = self.ai_filter.next();
        self.recompute();
        self.status = format!("AI filter: {}", self.ai_filter.label());
    }

    /// Open the technology picker, populated from what's actually in the
    /// data rather than from the full config.
    ///
    /// Showing only tags that appear, with counts, is the difference
    /// between a useful list and 45 entries most of which match nothing —
    /// and the counts themselves answer "AWS or GCP?" before you even
    /// pick one.
    pub fn open_tech_picker(&mut self) {
        let mut counts: std::collections::HashMap<crate::tech::Tag, usize> = Default::default();
        for row in &self.rows {
            if row.dismissed {
                continue;
            }
            for tag in &row.tech {
                *counts.entry(tag.clone()).or_default() += 1;
            }
        }
        let mut choices: Vec<_> = counts.into_iter().collect();
        choices.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        self.tech_choices = choices;
        self.tech_cursor = 0;
        self.tech_query.clear();
        self.mode = Mode::TechPicker;
    }

    /// The picker rows matching what's been typed so far.
    pub fn tech_visible(&self) -> Vec<(crate::tech::Tag, usize)> {
        let q = self.tech_query.to_lowercase();
        self.tech_choices
            .iter()
            .filter(|(tag, _)| q.is_empty() || tag.qualified().contains(&q))
            .cloned()
            .collect()
    }

    pub fn tech_move(&mut self, delta: isize) {
        let len = self.tech_visible().len();
        if len == 0 {
            return;
        }
        self.tech_cursor = (self.tech_cursor as isize + delta).clamp(0, len as isize - 1) as usize;
    }

    pub fn tech_select(&mut self) {
        let visible = self.tech_visible();
        if let Some((tag, n)) = visible.get(self.tech_cursor) {
            self.status = format!("tech: {} ({n} postings)", tag.qualified());
            self.tech_filter = Some(tag.clone());
        }
        self.mode = Mode::Normal;
        self.recompute();
    }

    pub fn clear_tech_filter(&mut self) {
        self.tech_filter = None;
        self.mode = Mode::Normal;
        self.status = "tech filter cleared".into();
        self.recompute();
    }

    pub fn tech_label(&self) -> String {
        self.tech_filter
            .as_ref()
            .map(|t| t.name.clone())
            .unwrap_or_else(|| "any".into())
    }

    pub fn cycle_level_filter(&mut self) {
        self.level_filter = self.level_filter.next();
        self.recompute();
        // The breakdown rides along with the message because cycling to a
        // rung is exactly when you want to know how many are there — an
        // empty "junior" view is otherwise indistinguishable from a broken
        // filter.
        let tally = self
            .level_counts()
            .iter()
            .map(|(label, n)| format!("{n} {label}"))
            .collect::<Vec<_>>()
            .join(" · ");
        let head = match self.level_filter {
            LevelFilter::Policy => format!(
                "level: {} (the filters.yaml policy)",
                self.seniority_policy.describe()
            ),
            other => format!("level: {}", other.label()),
        };
        self.status = if tally.is_empty() {
            head
        } else {
            format!("{head}   [{tally}]")
        };
    }

    pub fn cycle_my_filter(&mut self) {
        self.my_filter = self.my_filter.next();
        self.recompute();
        self.status = format!("status filter: {}", self.my_filter.label());
    }

    pub fn cycle_sort(&mut self) {
        self.sort = self.sort.next();
        // Status sorts ascending so the live end of the pipeline
        // (interview, then applied) is at the top rather than the bottom.
        self.sort_desc = matches!(self.sort, SortKey::Score | SortKey::Posted | SortKey::Age);
        self.recompute();
        self.status = format!(
            "sorted by {} {}",
            self.sort.label(),
            if self.sort_desc { "desc" } else { "asc" }
        );
    }

    pub fn toggle_sort_dir(&mut self) {
        self.sort_desc = !self.sort_desc;
        self.recompute();
    }

    /// Set (or with `None`, clear) the selected job's status, writing
    /// through to SQLite immediately.
    ///
    /// Writing on every keystroke rather than batching is deliberate: this
    /// is the one piece of state in the app that the user authored, the
    /// frozen web board reads the same table, and a TUI that loses your
    /// triage because it crashed before a flush would be worse than useless.
    pub fn set_status(&mut self, store: &Store, status: Option<&str>) -> Result<()> {
        let Some(row) = self.selected_row() else {
            return Ok(());
        };
        let url = row.url.clone();
        let notes = row.notes.clone();
        store.save_user_status(&url, status, notes.as_deref())?;

        if let Some(row) = self.rows.iter_mut().find(|r| r.url == url) {
            row.my_status = status.filter(|s| !s.is_empty()).map(str::to_string);
        }
        self.status = match status.filter(|s| !s.is_empty()) {
            Some(s) => format!("marked {s}"),
            None => "status cleared".to_string(),
        };
        // The row may no longer belong in the current view (marking a job
        // "skipped" while filtered to untouched, say) — recompute so it
        // leaves, with the cursor landing on a real neighbour.
        self.recompute();
        Ok(())
    }

    pub fn save_notes(&mut self, store: &Store) -> Result<()> {
        let Some(row) = self.selected_row() else {
            return Ok(());
        };
        let url = row.url.clone();
        let my_status = row.my_status.clone();
        let notes = self.input.clone();
        store.save_user_status(&url, my_status.as_deref(), Some(notes.as_str()))?;
        if let Some(row) = self.rows.iter_mut().find(|r| r.url == url) {
            row.notes = (!notes.is_empty()).then_some(notes);
        }
        self.status = "notes saved".to_string();
        Ok(())
    }

    pub fn open_selected(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        let url = row.url.clone();
        // A failed browser launch is a status-line message, never a crash:
        // this runs on headless boxes and over SSH, where there is no
        // browser to launch and the URL on screen is the fallback.
        match open::that_detached(&url) {
            Ok(()) => self.status = format!("opened {url}"),
            Err(e) => self.status = format!("couldn't open browser ({e}) — {url}"),
        }
    }

    /// Open a blank manual-entry form.
    pub fn open_add_form(&mut self) {
        let mut form = super::form::Form::new();
        // Seed the search box's text as the company: typing `/acme` then
        // finding nothing is the most common way to discover a job isn't
        // tracked yet, and re-typing the name is pure friction.
        if !self.search.is_empty() && self.view.is_empty() {
            form.set(super::form::FieldId::Company, &self.search);
        }
        self.form = Some(form);
        self.mode = Mode::Form;
    }

    /// Open the form pre-filled from the selected row.
    ///
    /// Any row can be edited, not just manual ones: correcting a mangled
    /// title or adding a stage to a scraped posting is useful, and
    /// `upsert_manual_job` merges rather than overwriting, keeping the
    /// original source.
    pub fn open_edit_form(&mut self) {
        let Some(row) = self.selected_row() else {
            self.status = "nothing selected".into();
            return;
        };
        self.form = Some(super::form::Form::from_row(row));
        self.mode = Mode::Form;
    }

    /// Validate and save the open form. Returns false if it didn't
    /// validate, leaving the form open with the error shown.
    pub fn save_form(&mut self, store: &Store) -> Result<bool> {
        let Some(form) = &mut self.form else {
            return Ok(false);
        };
        let entry = match form.to_entry() {
            Ok(entry) => entry,
            Err(e) => {
                form.error = Some(e);
                return Ok(false);
            }
        };
        let created = store.upsert_manual_job(&entry)?;
        let url = entry.url.clone();
        self.form = None;
        self.mode = Mode::Normal;
        self.reload(store)?;
        // Land the cursor on what was just saved, so the next keystroke
        // acts on it — otherwise a new entry vanishes into a list of 800.
        if let Some(pos) = self.view.iter().position(|&i| self.rows[i].url == url) {
            self.selected = pos;
        }
        self.status = if created {
            format!("added {} — {}", entry.company, entry.title)
        } else {
            format!("updated {} — {}", entry.company, entry.title)
        };
        Ok(true)
    }

    pub fn cancel_form(&mut self) {
        self.form = None;
        self.mode = Mode::Normal;
        self.status = "cancelled".into();
    }

    /// Delete the selected row, but only if it's a manual one — deleting a
    /// fetched row would just invite it straight back on the next run,
    /// which looks like the delete silently failing.
    pub fn delete_selected(&mut self, store: &Store) -> Result<()> {
        let Some(row) = self.selected_row() else {
            return Ok(());
        };
        if !row.is_manual() {
            self.status = "only hand-entered rows can be deleted".into();
            self.mode = Mode::Normal;
            return Ok(());
        }
        let (url, company, title) = (row.url.clone(), row.company.clone(), row.title.clone());
        store.delete_job(&url)?;
        self.mode = Mode::Normal;
        self.reload(store)?;
        self.status = format!("deleted {company} — {title}");
        Ok(())
    }

    /// `dd` / `D` — ask to delete the selected row, if it's one we're
    /// willing to delete.
    pub fn request_delete(&mut self) {
        match self.selected_row() {
            Some(row) if row.is_manual() => self.mode = Mode::ConfirmDelete,
            Some(_) => self.status = "only hand-entered rows can be deleted".into(),
            None => {}
        }
    }

    /// How many rows sit at each rung, for the header.
    pub fn level_counts(&self) -> Vec<(&'static str, usize)> {
        Seniority::ALL
            .iter()
            .map(|&level| {
                (
                    level.short(),
                    self.rows
                        .iter()
                        .filter(|r| r.seniority() == Some(level))
                        .count(),
                )
            })
            .filter(|(_, n)| *n > 0)
            .collect()
    }

    /// `x` — ask to hide this posting.
    pub fn request_dismiss_job(&mut self) {
        match self.selected_row() {
            Some(row) if row.dismissed => self.status = "already dismissed — u restores it".into(),
            Some(_) => self.mode = Mode::ConfirmDismissJob,
            None => {}
        }
    }

    /// `X` — ask to hide every posting from this employer.
    pub fn request_dismiss_company(&mut self) {
        if self.selected_row().is_some() {
            self.mode = Mode::ConfirmDismissCompany;
        }
    }

    pub fn dismiss_selected_job(&mut self, store: &Store) -> Result<()> {
        let Some(row) = self.selected_row().cloned() else {
            self.mode = Mode::Normal;
            return Ok(());
        };
        store.dismiss_job(&row, None)?;
        self.mode = Mode::Normal;
        self.reload(store)?;
        self.status = format!(
            "dismissed {} — {}   (m to the dismissed view, u to undo)",
            row.company, row.title
        );
        Ok(())
    }

    pub fn dismiss_selected_company(&mut self, store: &Store) -> Result<()> {
        let Some(row) = self.selected_row().cloned() else {
            self.mode = Mode::Normal;
            return Ok(());
        };
        let n = store.dismiss_company(&row.company, None)?;
        self.mode = Mode::Normal;
        self.reload(store)?;
        self.status = format!(
            "dismissed all of {} — {n} posting(s) hidden   (m to the dismissed view, u to undo)",
            row.company
        );
        Ok(())
    }

    /// `u` — put the selected row back.
    ///
    /// Restores whichever dismissal is actually hiding it: undoing a
    /// company-wide block from one of its postings is the common case, and
    /// deleting only the job-level row would leave it stubbornly hidden.
    pub fn restore_selected(&mut self, store: &Store) -> Result<()> {
        let Some(row) = self.selected_row().cloned() else {
            return Ok(());
        };
        if !row.dismissed {
            self.status = "not dismissed".into();
            return Ok(());
        }
        match row.dismissed_scope {
            Some("company") => {
                store.restore_company(&row.company)?;
                self.status = format!("restored everything from {}", row.company);
            }
            _ => {
                store.restore_job(&row.url)?;
                self.status = format!("restored {} — {}", row.company, row.title);
            }
        }
        self.reload(store)?;
        Ok(())
    }

    /// How many rows the next batch scoring run would pay for.
    pub fn unscored_in_scope(&self) -> usize {
        self.rows
            .iter()
            .filter(|r| {
                r.recommendation.is_none()
                    && !r.dismissed
                    && (!self.startup_only || self.startup_policy.admits(r))
                    && self.seniority_policy.admits(r)
            })
            .count()
    }

    /// Days past its threshold, or None if this row isn't stale.
    pub fn staleness_days(&self, row: &JobRow) -> Option<i64> {
        let status = row.my_status.as_deref()?;
        let threshold = *self.staleness.get(status)?;
        let age = row.status_age_days()?;
        (age >= threshold).then_some(age - threshold)
    }

    pub fn is_stale(&self, row: &JobRow) -> bool {
        self.staleness_days(row).is_some()
    }

    pub fn stale_count(&self) -> usize {
        self.rows
            .iter()
            .filter(|r| !r.dismissed && self.is_stale(r))
            .count()
    }

    pub fn dismissed_count(&self) -> usize {
        self.rows.iter().filter(|r| r.dismissed).count()
    }

    pub fn manual_count(&self) -> usize {
        self.rows.iter().filter(|r| r.is_manual()).count()
    }

    /// Reload every row from SQLite, keeping the cursor on the same job.
    pub fn reload(&mut self, store: &Store) -> Result<()> {
        self.rows = store.all_candidates()?;
        self.recompute();
        Ok(())
    }

    // ------------------------------------------------------------ counts

    pub fn startup_count(&self) -> usize {
        self.rows
            .iter()
            .filter(|r| self.startup_policy.admits(r))
            .count()
    }

    pub fn unscored_count(&self) -> usize {
        self.rows
            .iter()
            .filter(|r| r.recommendation.is_none())
            .count()
    }

    /// How many rows carry each status, for the header's tracker summary.
    pub fn status_counts(&self) -> Vec<(&'static str, usize)> {
        MY_STATUS_VALUES
            .iter()
            .map(|&status| {
                (
                    status,
                    self.rows
                        .iter()
                        .filter(|r| r.my_status.as_deref() == Some(status))
                        .count(),
                )
            })
            .filter(|(_, n)| *n > 0)
            .collect()
    }

    pub fn status_key(index: usize) -> Option<&'static str> {
        MY_STATUS_VALUES.get(index).copied()
    }
}

/// Sort order for `my_status`: where it sits in the pipeline, not the
/// alphabet. Interview first because that's the one with a date attached;
/// untouched last because it's the absence of a decision, and burying it
/// beats having 800 unmarked rows head a status sort.
fn status_rank(status: Option<&str>) -> u8 {
    match status {
        // Furthest along first. A recruiter screen outranks a bare
        // application because a human has actually replied.
        Some("interview") => 0,
        Some("recruiter") => 1,
        Some("applied") => 2,
        Some("silence") => 3,
        Some("rejected") => 4,
        Some("skipped") => 5,
        _ => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Stage, StageSource};

    fn row(company: &str, score: Option<i64>, stage: Option<Stage>) -> JobRow {
        JobRow {
            url: format!("https://example.com/{company}"),
            company: company.into(),
            title: format!("{company} Engineer"),
            location: "Remote - US".into(),
            match_score: score,
            recommendation: score.map(|s| {
                if s >= 80 {
                    "apply".into()
                } else {
                    "consider".into()
                }
            }),
            stage,
            stage_source: stage.map(|_| StageSource::Curated),
            ..Default::default()
        }
    }

    fn level_policy() -> SeniorityPolicy {
        SeniorityPolicy {
            min: Seniority::Junior,
            max: Seniority::Staff,
            include_unknown: true,
        }
    }

    fn policy() -> StartupPolicy {
        StartupPolicy {
            enabled_by_default: true,
            max_stage: Stage::Growth,
            max_headcount: None,
            include_unknown: false,
        }
    }

    fn app() -> App {
        App::new(
            vec![
                row("Fly", Some(92), Some(Stage::SeriesB)),
                row("Amazon", Some(60), Some(Stage::BigTech)),
                row("Acme", None, Some(Stage::Seed)),
                row("Mystery", Some(70), None),
            ],
            policy(),
            level_policy(),
            Default::default(),
        )
    }

    #[test]
    fn startup_mode_is_on_by_default_and_toggles_off() {
        let mut a = app();
        assert!(a.startup_only);
        let names: Vec<_> = a.view.iter().map(|&i| a.rows[i].company.clone()).collect();
        assert!(names.contains(&"Fly".to_string()));
        assert!(names.contains(&"Acme".to_string()));
        assert!(
            !names.contains(&"Amazon".to_string()),
            "big tech filtered out"
        );
        assert!(
            !names.contains(&"Mystery".to_string()),
            "unknown stage filtered out"
        );

        a.toggle_startup();
        assert_eq!(a.view.len(), 4, "every stage visible with startup mode off");
    }

    #[test]
    fn selection_follows_the_job_not_the_index_when_the_view_changes() {
        let mut a = app();
        a.toggle_startup(); // all four visible, sorted by score desc
                            // Put the cursor on Amazon specifically.
        let amazon = a
            .view
            .iter()
            .position(|&i| a.rows[i].company == "Amazon")
            .unwrap();
        a.selected = amazon;
        assert_eq!(a.selected_row().unwrap().company, "Amazon");

        // Sorting changes every index; the cursor must stay on Amazon.
        a.cycle_sort();
        assert_eq!(a.selected_row().unwrap().company, "Amazon");
    }

    #[test]
    fn selection_stays_in_bounds_when_the_anchor_leaves_the_view() {
        let mut a = app();
        a.toggle_startup();
        let amazon = a
            .view
            .iter()
            .position(|&i| a.rows[i].company == "Amazon")
            .unwrap();
        a.selected = amazon;
        // Turning startup mode back on removes the selected row entirely.
        a.toggle_startup();
        assert!(a.selected < a.view.len(), "selection must stay in bounds");
    }

    #[test]
    fn unscored_sorts_below_a_genuine_zero() {
        let mut a = App::new(
            vec![
                row("Zero", Some(0), Some(Stage::Seed)),
                row("Unscored", None, Some(Stage::Seed)),
            ],
            policy(),
            level_policy(),
            Default::default(),
        );
        a.sort = SortKey::Score;
        a.sort_desc = true;
        a.recompute();
        let order: Vec<_> = a.view.iter().map(|&i| a.rows[i].company.clone()).collect();
        assert_eq!(order, vec!["Zero", "Unscored"]);
    }

    #[test]
    fn ai_filter_unscored_selects_only_unevaluated_rows() {
        let mut a = app();
        a.toggle_startup();
        a.ai_filter = AiFilter::Unscored;
        a.recompute();
        assert_eq!(a.view.len(), 1);
        assert_eq!(a.rows[a.view[0]].company, "Acme");
    }

    #[test]
    fn search_matches_company_title_and_location() {
        let mut a = app();
        a.toggle_startup();
        a.search = "amaz".into();
        a.recompute();
        assert_eq!(a.view.len(), 1);
        a.search = "remote".into();
        a.recompute();
        assert_eq!(a.view.len(), 4, "location is searchable too");
    }

    #[test]
    fn recruiter_sorts_between_applied_and_interview() {
        let with_status = |company: &str, status: &str| JobRow {
            url: format!("https://x/{company}"),
            company: company.into(),
            my_status: Some(status.into()),
            stage: Some(Stage::Seed),
            ..Default::default()
        };
        let mut a = App::new(
            vec![
                with_status("Applied", "applied"),
                with_status("Interview", "interview"),
                with_status("Recruiter", "recruiter"),
            ],
            policy(),
            level_policy(),
            Default::default(),
        );
        a.sort = SortKey::Status;
        a.sort_desc = false;
        a.recompute();
        let order: Vec<_> = a.view.iter().map(|&i| a.rows[i].company.clone()).collect();
        assert_eq!(
            order,
            vec!["Interview", "Recruiter", "Applied"],
            "a recruiter screen is further along than a bare application"
        );
    }

    #[test]
    fn status_sorts_by_pipeline_position_not_alphabetically() {
        let with_status = |company: &str, status: Option<&str>| JobRow {
            url: format!("https://x/{company}"),
            company: company.into(),
            my_status: status.map(str::to_string),
            stage: Some(Stage::Seed),
            ..Default::default()
        };
        let mut a = App::new(
            vec![
                with_status("Untouched", None),
                with_status("Rejected", Some("rejected")),
                with_status("Interview", Some("interview")),
                with_status("Applied", Some("applied")),
            ],
            policy(),
            level_policy(),
            Default::default(),
        );
        a.sort = SortKey::Status;
        a.sort_desc = false;
        a.recompute();
        let order: Vec<_> = a.view.iter().map(|&i| a.rows[i].company.clone()).collect();
        assert_eq!(
            order,
            vec!["Interview", "Applied", "Rejected", "Untouched"],
            "live end of the pipeline first, absence of a decision last"
        );
    }

    #[test]
    fn tracked_filter_shows_everything_you_have_acted_on() {
        let with_status = |company: &str, status: Option<&str>| JobRow {
            url: format!("https://x/{company}"),
            company: company.into(),
            my_status: status.map(str::to_string),
            stage: Some(Stage::Seed),
            ..Default::default()
        };
        let mut a = App::new(
            vec![
                with_status("A", Some("applied")),
                with_status("B", Some("rejected")),
                with_status("C", None),
            ],
            policy(),
            level_policy(),
            Default::default(),
        );
        a.my_filter = MyFilter::Tracked;
        a.recompute();
        assert_eq!(a.view.len(), 2);

        a.my_filter = MyFilter::Untouched;
        a.recompute();
        assert_eq!(a.view.len(), 1);
    }

    #[test]
    fn dismissed_rows_are_invisible_to_every_filter_but_the_dismissed_one() {
        let hidden = |company: &str, dismissed: bool| JobRow {
            url: format!("https://x/{company}"),
            company: company.into(),
            my_status: Some("applied".into()),
            stage: Some(Stage::Seed),
            dismissed,
            dismissed_scope: dismissed.then_some("job"),
            ..Default::default()
        };
        let mut a = App::new(
            vec![hidden("Visible", false), hidden("Hidden", true)],
            policy(),
            level_policy(),
            Default::default(),
        );

        for filter in [MyFilter::All, MyFilter::Tracked, MyFilter::Applied] {
            a.my_filter = filter;
            a.recompute();
            assert_eq!(a.view.len(), 1, "{:?} must not show dismissed rows", filter);
            assert_eq!(a.rows[a.view[0]].company, "Visible");
        }

        a.my_filter = MyFilter::Dismissed;
        a.recompute();
        assert_eq!(a.view.len(), 1);
        assert_eq!(a.rows[a.view[0]].company, "Hidden");
    }

    #[test]
    fn the_stale_filter_shows_only_rows_past_their_own_threshold() {
        let aged = |company: &str, status: &str, days: i64| {
            let at = chrono::Utc::now() - chrono::Duration::days(days);
            JobRow {
                company: company.into(),
                my_status: Some(status.into()),
                status_since: Some(at.format("%Y-%m-%d %H:%M:%S").to_string()),
                // The test policy excludes unknown stages, and this test
                // is about staleness, not startup mode.
                stage: Some(Stage::Seed),
                ..Default::default()
            }
        };
        let mut a = App::new(
            vec![
                aged("Overdue", "recruiter", 30),
                aged("Fresh", "recruiter", 2),
                // Past 30 days, but `rejected` has no threshold at all —
                // a closed door doesn't need chasing.
                aged("Closed", "rejected", 300),
                JobRow {
                    company: "Untouched".into(),
                    stage: Some(Stage::Seed),
                    ..Default::default()
                },
            ],
            policy(),
            level_policy(),
            [("recruiter".to_string(), 14)].into_iter().collect(),
        );
        a.my_filter = MyFilter::Stale;
        a.recompute();
        assert_eq!(a.view.len(), 1);
        assert_eq!(a.rows[a.view[0]].company, "Overdue");
        assert_eq!(a.stale_count(), 1);
    }

    #[test]
    fn the_status_filter_cycle_order_matches_what_the_help_advertises() {
        let mut order = vec![];
        let mut f = MyFilter::All;
        loop {
            order.push(f.label());
            f = f.next();
            if f == MyFilter::All {
                break;
            }
        }
        assert_eq!(
            order,
            vec![
                "all",
                "tracked",
                "stale",
                "dismissed",
                "untouched",
                "applied",
                "recruiter",
                "interview",
                "rejected",
                "skipped",
                "silence"
            ],
            "the help overlay lists this order; they must not drift"
        );
    }

    #[test]
    fn the_status_filter_cycle_reaches_every_value() {
        let mut seen = vec![];
        let mut f = MyFilter::All;
        for _ in 0..16 {
            seen.push(f.label());
            f = f.next();
            if f == MyFilter::All {
                break;
            }
        }
        for expected in [
            "all",
            "tracked",
            "untouched",
            "applied",
            "interview",
            "rejected",
            "skipped",
            "silence",
        ] {
            assert!(seen.contains(&expected), "cycle must reach {expected}");
        }
    }

    #[test]
    fn navigation_clamps_at_both_ends() {
        let mut a = app();
        a.move_selection(-5);
        assert_eq!(a.selected, 0);
        a.move_selection(100);
        assert_eq!(a.selected, a.view.len() - 1);
    }

    #[test]
    fn status_edits_write_through_and_update_the_row() {
        let store = Store::open(":memory:").unwrap();
        let mut a = app();
        a.toggle_startup();
        a.selected = 0;
        let url = a.selected_row().unwrap().url.clone();
        a.set_status(&store, Some("applied")).unwrap();
        assert_eq!(
            a.rows
                .iter()
                .find(|r| r.url == url)
                .unwrap()
                .my_status
                .as_deref(),
            Some("applied")
        );
        a.selected = a.view.iter().position(|&i| a.rows[i].url == url).unwrap();
        a.set_status(&store, None).unwrap();
        assert_eq!(
            a.rows.iter().find(|r| r.url == url).unwrap().my_status,
            None
        );
    }
}
