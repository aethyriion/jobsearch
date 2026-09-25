//! The manual-entry form: a full-screen page for recording an application
//! the scrapers never found, or won't find again.
//!
//! It collects exactly the fields a fetcher produces, plus the two that
//! only you can know (your status, your notes) and the stage, so a
//! hand-entered row is indistinguishable from a scraped one everywhere
//! downstream — it shows inline in the list, and the AI step scores it
//! like anything else.
//!
//! Kept free of terminal I/O so the field mechanics are testable: `ui.rs`
//! draws it, `mod.rs` feeds it key events.

use crate::model::{Stage, MY_STATUS_VALUES};
use crate::store::ManualEntry;

/// What a field accepts. The distinction drives both rendering and key
/// handling — a choice field ignores typing, a multiline field treats
/// Enter as a newline rather than as "next field".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    /// Cycled with ←/→ rather than typed.
    Choice,
    Multiline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldId {
    Company,
    Title,
    Location,
    Url,
    Posted,
    Stage,
    Headcount,
    Status,
    Notes,
    Description,
}

pub struct FieldSpec {
    pub id: FieldId,
    pub label: &'static str,
    pub kind: FieldKind,
    pub hint: &'static str,
}

/// Field order is the order you'd fill them in from a job posting open in
/// another window: who and what first, then where, then the paperwork.
pub const FIELDS: [FieldSpec; 10] = [
    FieldSpec {
        id: FieldId::Company,
        label: "Company",
        kind: FieldKind::Text,
        hint: "required",
    },
    FieldSpec {
        id: FieldId::Title,
        label: "Title",
        kind: FieldKind::Text,
        hint: "required",
    },
    FieldSpec {
        id: FieldId::Location,
        label: "Location",
        kind: FieldKind::Text,
        hint: "e.g. Remote - US",
    },
    FieldSpec {
        id: FieldId::Url,
        label: "URL",
        kind: FieldKind::Text,
        hint: "blank = generate one",
    },
    FieldSpec {
        id: FieldId::Posted,
        label: "Posted",
        kind: FieldKind::Text,
        hint: "YYYY-MM-DD",
    },
    FieldSpec {
        id: FieldId::Stage,
        label: "Stage",
        kind: FieldKind::Choice,
        hint: "←/→ to cycle",
    },
    FieldSpec {
        id: FieldId::Headcount,
        label: "Headcount",
        kind: FieldKind::Text,
        hint: "approx people",
    },
    FieldSpec {
        id: FieldId::Status,
        label: "Status",
        kind: FieldKind::Choice,
        hint: "←/→ to cycle",
    },
    FieldSpec {
        id: FieldId::Notes,
        label: "Notes",
        kind: FieldKind::Text,
        hint: "why you applied, who referred you",
    },
    FieldSpec {
        id: FieldId::Description,
        label: "Description",
        kind: FieldKind::Multiline,
        hint: "paste the JD — the AI step scores against this",
    },
];

/// "unset" is a real choice, not a placeholder: plenty of applications
/// have no stage you know and no status yet, and forcing a value would put
/// a guess into the data.
const UNSET: &str = "—";

pub struct Form {
    /// Parallel to FIELDS.
    values: [String; 10],
    pub focus: usize,
    /// Set when editing an existing row; its URL is the identity to write
    /// back to, even if the URL box is later edited.
    pub editing: Option<String>,
    pub error: Option<String>,
    /// Scroll offset for the description box.
    pub desc_scroll: u16,
}

impl Default for Form {
    fn default() -> Self {
        Self::new()
    }
}

impl Form {
    pub fn new() -> Self {
        let mut values: [String; 10] = Default::default();
        values[index_of(FieldId::Stage)] = UNSET.to_string();
        values[index_of(FieldId::Status)] = UNSET.to_string();
        Self {
            values,
            focus: 0,
            editing: None,
            error: None,
            desc_scroll: 0,
        }
    }

    /// Pre-fill from an existing row, for editing.
    pub fn from_row(row: &crate::model::JobRow) -> Self {
        let mut form = Self::new();
        form.set(FieldId::Company, &row.company);
        form.set(FieldId::Title, &row.title);
        form.set(FieldId::Location, &row.location);
        form.set(FieldId::Url, &row.url);
        form.set(FieldId::Posted, row.posted_at.as_deref().unwrap_or(""));
        form.set(
            FieldId::Stage,
            row.stage.map(Stage::as_str).unwrap_or(UNSET),
        );
        form.set(
            FieldId::Headcount,
            &row.headcount.map(|h| h.to_string()).unwrap_or_default(),
        );
        form.set(FieldId::Status, row.my_status.as_deref().unwrap_or(UNSET));
        form.set(FieldId::Notes, row.notes.as_deref().unwrap_or(""));
        // The stored description may be HTML from a fetcher; show it as
        // the text the model will actually see rather than as markup.
        //
        // Note this is lossy for a fetched row: saving writes the
        // flattened text back over the stored HTML. Nothing in this binary
        // reads that markup (both the scorer and the detail pane strip it
        // anyway), and seeing exactly what the model sees is worth more
        // than preserving tags for the frozen web board.
        form.set(FieldId::Description, &flatten_description(&row.description));
        form.editing = Some(row.url.clone());
        form
    }

    pub fn get(&self, id: FieldId) -> &str {
        &self.values[index_of(id)]
    }

    pub fn set(&mut self, id: FieldId, value: &str) {
        self.values[index_of(id)] = value.to_string();
    }

    pub fn focused(&self) -> &FieldSpec {
        &FIELDS[self.focus]
    }

    pub fn value_at(&self, i: usize) -> &str {
        &self.values[i]
    }

    pub fn title(&self) -> &'static str {
        if self.editing.is_some() {
            " Edit entry "
        } else {
            " Add application "
        }
    }

    // ------------------------------------------------------------ editing

    pub fn next_field(&mut self) {
        self.focus = (self.focus + 1) % FIELDS.len();
        self.desc_scroll = 0;
    }

    pub fn prev_field(&mut self) {
        self.focus = (self.focus + FIELDS.len() - 1) % FIELDS.len();
        self.desc_scroll = 0;
    }

    pub fn insert_char(&mut self, c: char) {
        if self.focused().kind == FieldKind::Choice {
            return;
        }
        let i = self.focus;
        self.values[i].push(c);
        self.error = None;
    }

    /// Bulk insert, for a bracketed paste. This is the whole reason
    /// bracketed paste is enabled: a job description arrives as one event
    /// instead of as a thousand synthetic keystrokes, which is both
    /// instant and immune to a stray character in the JD being read as a
    /// command.
    pub fn insert_str(&mut self, text: &str) {
        if self.focused().kind == FieldKind::Choice {
            return;
        }
        let i = self.focus;
        // Normalize line endings first either way: folding CRLF with a
        // blanket char replace turns one break into two spaces.
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        let cleaned = if self.focused().kind == FieldKind::Multiline {
            normalized
        } else {
            // A newline pasted into a single-line box would be invisible
            // and would corrupt the value; fold it to a space.
            normalized.replace('\n', " ")
        };
        self.values[i].push_str(&cleaned);
        self.error = None;
    }

    pub fn backspace(&mut self) {
        if self.focused().kind == FieldKind::Choice {
            return;
        }
        let i = self.focus;
        self.values[i].pop();
        self.error = None;
    }

    pub fn newline(&mut self) {
        if self.focused().kind == FieldKind::Multiline {
            let i = self.focus;
            self.values[i].push('\n');
        } else {
            self.next_field();
        }
    }

    /// Cycle a choice field. `delta` is +1 or -1; the list wraps, and
    /// includes UNSET so you can always get back to "no answer".
    pub fn cycle(&mut self, delta: isize) {
        let options: Vec<String> = match self.focused().id {
            FieldId::Stage => std::iter::once(UNSET.to_string())
                .chain(Stage::ALL.iter().map(|s| s.as_str().to_string()))
                .collect(),
            FieldId::Status => std::iter::once(UNSET.to_string())
                .chain(MY_STATUS_VALUES.iter().map(|s| s.to_string()))
                .collect(),
            _ => return,
        };
        let i = self.focus;
        let current = options
            .iter()
            .position(|o| o == &self.values[i])
            .unwrap_or(0) as isize;
        let next = (current + delta).rem_euclid(options.len() as isize) as usize;
        self.values[i] = options[next].clone();
    }

    pub fn scroll_description(&mut self, delta: i16) {
        self.desc_scroll = self.desc_scroll.saturating_add_signed(delta);
    }

    // ------------------------------------------------------------ output

    /// Validate and convert. Company and title are the only hard
    /// requirements — everything else is genuinely optional, and a
    /// half-remembered application is still worth recording.
    pub fn to_entry(&self) -> Result<ManualEntry, String> {
        let company = self.get(FieldId::Company).trim().to_string();
        let title = self.get(FieldId::Title).trim().to_string();
        if company.is_empty() {
            return Err("Company is required".into());
        }
        if title.is_empty() {
            return Err("Title is required".into());
        }

        let headcount_raw = self.get(FieldId::Headcount).trim();
        let headcount = if headcount_raw.is_empty() {
            None
        } else {
            match headcount_raw.parse::<i64>() {
                Ok(h) if h >= 0 => Some(h),
                _ => return Err(format!("Headcount {headcount_raw:?} isn't a number")),
            }
        };

        let posted = self.get(FieldId::Posted).trim();
        if !posted.is_empty() && !looks_like_date(posted) {
            return Err(format!("Posted {posted:?} isn't a YYYY-MM-DD date"));
        }

        let choice = |id: FieldId| -> Option<String> {
            let v = self.get(id).trim();
            (!v.is_empty() && v != UNSET).then(|| v.to_string())
        };

        let entry = ManualEntry {
            // When editing, the row's original URL wins over the box:
            // changing the URL of an existing entry would otherwise
            // silently create a second row rather than move the first.
            url: self
                .editing
                .clone()
                .unwrap_or_else(|| self.get(FieldId::Url).trim().to_string()),
            company,
            title,
            location: self.get(FieldId::Location).trim().to_string(),
            posted_at: posted.to_string(),
            description: self.get(FieldId::Description).trim().to_string(),
            stage: choice(FieldId::Stage).and_then(|s| Stage::parse(&s)),
            headcount,
            my_status: choice(FieldId::Status),
            // Always Some so a cleared box actually clears the stored
            // note, rather than the blank meaning "don't touch it".
            notes: Some(self.get(FieldId::Notes).trim().to_string()),
        };
        Ok(entry.with_url())
    }
}

/// HTML to clean, editable text.
///
/// `strip_html` turns every tag into a space so adjacent words don't fuse,
/// which leaves runs of spaces wherever tags were adjacent. Harmless for
/// keyword matching, ugly in a text box you're about to edit — so squeeze
/// them here rather than changing the shared helper the filters depend on.
fn flatten_description(raw: &str) -> String {
    let text = crate::filters::strip_html(raw);
    let squeezed = text
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n");
    crate::filters::collapse_whitespace(&squeezed)
}

fn index_of(id: FieldId) -> usize {
    FIELDS
        .iter()
        .position(|f| f.id == id)
        .expect("every FieldId appears in FIELDS")
}

/// Loose date check: the point is to catch "last tuesday" and "9/20",
/// not to validate the calendar. A wrong-but-well-formed date is the
/// user's business.
fn looks_like_date(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 3
        && parts[0].len() == 4
        && parts[1].len() == 2
        && parts[2].len() == 2
        && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{JobRow, MANUAL_SOURCE};

    fn filled() -> Form {
        let mut f = Form::new();
        f.set(FieldId::Company, "Acme Corp");
        f.set(FieldId::Title, "Staff Platform Engineer");
        f
    }

    #[test]
    fn company_and_title_are_required_nothing_else_is() {
        let mut f = Form::new();
        assert!(f.to_entry().is_err());
        f.set(FieldId::Company, "Acme");
        assert!(f.to_entry().is_err(), "title still missing");
        f.set(FieldId::Title, "Engineer");
        let entry = f.to_entry().expect("company + title is enough");
        assert_eq!(entry.company, "Acme");
        assert!(entry.location.is_empty());
        assert!(entry.stage.is_none());
    }

    #[test]
    fn a_blank_url_gets_a_deterministic_synthetic_one() {
        let entry = filled().to_entry().unwrap();
        assert_eq!(entry.url, "manual:acme-corp/staff-platform-engineer");
        // Re-entering the same role produces the same URL, so it updates
        // rather than creating a near-duplicate.
        assert_eq!(filled().to_entry().unwrap().url, entry.url);
    }

    #[test]
    fn a_real_url_is_kept_verbatim() {
        let mut f = filled();
        f.set(FieldId::Url, "  https://boards.greenhouse.io/acme/jobs/1  ");
        assert_eq!(
            f.to_entry().unwrap().url,
            "https://boards.greenhouse.io/acme/jobs/1"
        );
    }

    #[test]
    fn choice_fields_cycle_and_wrap_through_unset() {
        let mut f = filled();
        f.focus = index_of(FieldId::Stage);
        assert_eq!(f.get(FieldId::Stage), UNSET);
        f.cycle(1);
        assert_eq!(f.get(FieldId::Stage), "pre-seed");
        f.cycle(-1);
        assert_eq!(f.get(FieldId::Stage), UNSET, "wraps back to unset");
        f.cycle(-1);
        assert_eq!(f.get(FieldId::Stage), "big-tech", "wraps to the end");
    }

    #[test]
    fn unset_choices_become_none_not_a_literal_dash() {
        let entry = filled().to_entry().unwrap();
        assert_eq!(entry.stage, None);
        assert_eq!(entry.my_status, None);
    }

    #[test]
    fn typing_is_ignored_on_choice_fields() {
        let mut f = filled();
        f.focus = index_of(FieldId::Stage);
        f.insert_char('x');
        f.insert_str("nonsense");
        assert_eq!(f.get(FieldId::Stage), UNSET);
    }

    #[test]
    fn pasting_into_a_single_line_field_folds_newlines() {
        let mut f = filled();
        f.focus = index_of(FieldId::Location);
        f.insert_str("Remote\r\n- US");
        assert_eq!(f.get(FieldId::Location), "Remote - US");
    }

    #[test]
    fn pasting_into_the_description_keeps_newlines() {
        let mut f = filled();
        f.focus = index_of(FieldId::Description);
        f.insert_str("Line one\r\nLine two\rLine three");
        assert_eq!(
            f.get(FieldId::Description),
            "Line one\nLine two\nLine three"
        );
    }

    #[test]
    fn enter_is_a_newline_in_the_description_and_a_move_elsewhere() {
        let mut f = filled();
        f.focus = index_of(FieldId::Description);
        f.newline();
        assert_eq!(f.get(FieldId::Description), "\n");
        assert_eq!(f.focus, index_of(FieldId::Description), "focus unchanged");

        f.focus = index_of(FieldId::Company);
        f.newline();
        assert_eq!(f.focus, index_of(FieldId::Title), "moves to the next field");
    }

    #[test]
    fn headcount_and_date_are_validated_with_useful_messages() {
        let mut f = filled();
        f.set(FieldId::Headcount, "about fifty");
        assert!(f.to_entry().unwrap_err().contains("isn't a number"));
        f.set(FieldId::Headcount, "50");
        assert_eq!(f.to_entry().unwrap().headcount, Some(50));

        f.set(FieldId::Posted, "last tuesday");
        assert!(f.to_entry().unwrap_err().contains("YYYY-MM-DD"));
        f.set(FieldId::Posted, "2026-09-20");
        assert!(f.to_entry().is_ok());
    }

    #[test]
    fn editing_a_row_round_trips_and_pins_the_original_url() {
        let row = JobRow {
            url: "manual:acme/engineer".into(),
            company: "Acme".into(),
            title: "Engineer".into(),
            location: "Remote - US".into(),
            description: "<p>Build <b>things</b></p>".into(),
            source: MANUAL_SOURCE.into(),
            my_status: Some("interview".into()),
            notes: Some("second round".into()),
            stage: Some(Stage::SeriesB),
            headcount: Some(80),
            ..Default::default()
        };
        let mut f = Form::from_row(&row);
        assert_eq!(f.get(FieldId::Stage), "series-b");
        assert_eq!(f.get(FieldId::Status), "interview");
        // HTML is flattened to what the model would actually read.
        assert_eq!(f.get(FieldId::Description), "Build things");

        // Editing the URL box must not fork the row.
        f.set(FieldId::Url, "https://somewhere.else/");
        assert_eq!(f.to_entry().unwrap().url, "manual:acme/engineer");
    }

    #[test]
    fn flattening_squeezes_the_spaces_tag_stripping_leaves_behind() {
        assert_eq!(
            flatten_description("<p>Build <b>things</b></p>"),
            "Build things"
        );
        assert_eq!(flatten_description("<li>One</li><li>Two</li>"), "One Two");
        // Paragraph breaks survive as a single blank line.
        assert_eq!(flatten_description("A\n\n\n\nB"), "A\n\nB");
    }

    #[test]
    fn clearing_the_notes_box_clears_the_stored_note() {
        let row = JobRow {
            url: "manual:a/b".into(),
            company: "A".into(),
            title: "B".into(),
            notes: Some("old note".into()),
            source: MANUAL_SOURCE.into(),
            ..Default::default()
        };
        let mut f = Form::from_row(&row);
        f.set(FieldId::Notes, "");
        assert_eq!(f.to_entry().unwrap().notes, Some(String::new()));
    }

    #[test]
    fn field_navigation_wraps_both_ways() {
        let mut f = Form::new();
        f.prev_field();
        assert_eq!(f.focus, FIELDS.len() - 1);
        f.next_field();
        assert_eq!(f.focus, 0);
    }
}
