//! TUI state and the transitions over it.
//!
//! Kept free of rendering and of terminal I/O on purpose: everything here
//! is a plain data transformation, so the filter/sort/selection rules are
//! testable without a terminal. `ui.rs` draws this state; `mod.rs` owns
//! the event loop and the background tasks.

use crate::filters::StartupPolicy;
use crate::model::{JobRow, MY_STATUS_VALUES};
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
}

impl SortKey {
    pub fn label(self) -> &'static str {
        match self {
            SortKey::Score => "score",
            SortKey::Company => "company",
            SortKey::Title => "title",
            SortKey::Stage => "stage",
            SortKey::Posted => "posted",
        }
    }

    pub fn next(self) -> Self {
        match self {
            SortKey::Score => SortKey::Company,
            SortKey::Company => SortKey::Title,
            SortKey::Title => SortKey::Stage,
            SortKey::Stage => SortKey::Posted,
            SortKey::Posted => SortKey::Score,
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
    Untouched,
    Applied,
    Interview,
    Rejected,
    Skipped,
    Silence,
}

impl MyFilter {
    pub fn label(self) -> &'static str {
        match self {
            MyFilter::All => "all",
            MyFilter::Untouched => "untouched",
            MyFilter::Applied => "applied",
            MyFilter::Interview => "interview",
            MyFilter::Rejected => "rejected",
            MyFilter::Skipped => "skipped",
            MyFilter::Silence => "silence",
        }
    }

    pub fn next(self) -> Self {
        match self {
            MyFilter::All => MyFilter::Untouched,
            MyFilter::Untouched => MyFilter::Applied,
            MyFilter::Applied => MyFilter::Interview,
            MyFilter::Interview => MyFilter::Rejected,
            MyFilter::Rejected => MyFilter::Skipped,
            MyFilter::Skipped => MyFilter::Silence,
            MyFilter::Silence => MyFilter::All,
        }
    }

    fn matches(self, row: &JobRow) -> bool {
        match self {
            MyFilter::All => true,
            MyFilter::Untouched => row.my_status.is_none(),
            other => row.my_status.as_deref() == Some(other.label()),
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
}

impl App {
    pub fn new(rows: Vec<JobRow>, startup_policy: StartupPolicy) -> Self {
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
            ai_filter: AiFilter::All,
            my_filter: MyFilter::All,
            search: String::new(),
            input: String::new(),
            detail_scroll: 0,
            status: String::new(),
            task: TaskState::default(),
            should_quit: false,
            table_state: TableState::default(),
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
                if !self.ai_filter.matches(row) {
                    return false;
                }
                if !self.my_filter.matches(row) {
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

    pub fn cycle_my_filter(&mut self) {
        self.my_filter = self.my_filter.next();
        self.recompute();
        self.status = format!("status filter: {}", self.my_filter.label());
    }

    pub fn cycle_sort(&mut self) {
        self.sort = self.sort.next();
        self.sort_desc = matches!(self.sort, SortKey::Score | SortKey::Posted);
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

    pub fn status_key(index: usize) -> Option<&'static str> {
        MY_STATUS_VALUES.get(index).copied()
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
