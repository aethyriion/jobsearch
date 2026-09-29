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

use crate::model::{Channel, Stage, MY_STATUS_VALUES};
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
    Channel,
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
pub const FIELDS: [FieldSpec; 11] = [
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
        id: FieldId::Channel,
        label: "Channel",
        kind: FieldKind::Choice,
        hint: "direct / agency / referral / inbound",
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

/// The form is modal, like the editor the rest of these keys come from.
/// It opens in Insert because you came here to type; Esc drops to Normal,
/// where `ZZ` writes and `:` opens the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormMode {
    Insert,
    Normal,
    Command,
}

/// What a key sequence asked the event loop to do. The form itself can't
/// touch the database, so `to_entry` + these are how a `:w` reaches it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormAction {
    None,
    /// Write and close (`:w`, `:wq`, `:x`, `ZZ`).
    Save,
    /// Close, discarding edits (`:q!`, `ZQ`).
    Discard,
    /// `:q` — refuses if there are unsaved edits, the way `:q` does.
    QuitIfClean,
}

pub struct Form {
    /// Parallel to FIELDS.
    values: [String; 11],
    pub mode: FormMode,
    /// The `:` line being typed, without its leading colon.
    pub command: String,
    /// First key of a two-key Normal-mode sequence (`gg`, `dd`, `ZZ`).
    pending: Option<char>,
    /// Set once any field is edited, so `:q` can refuse like vim's does.
    pub dirty: bool,
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
        let mut values: [String; 11] = Default::default();
        values[index_of(FieldId::Stage)] = UNSET.to_string();
        values[index_of(FieldId::Channel)] = UNSET.to_string();
        values[index_of(FieldId::Status)] = UNSET.to_string();
        Self {
            values,
            mode: FormMode::Insert,
            command: String::new(),
            pending: None,
            dirty: false,
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
        form.set(
            FieldId::Channel,
            row.channel.map(Channel::as_str).unwrap_or(UNSET),
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
        form.dirty = false;
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
        self.dirty = true;
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
        self.dirty = true;
        self.error = None;
    }

    pub fn backspace(&mut self) {
        if self.focused().kind == FieldKind::Choice {
            return;
        }
        let i = self.focus;
        self.values[i].pop();
        self.dirty = true;
        self.error = None;
    }

    pub fn newline(&mut self) {
        if self.focused().kind == FieldKind::Multiline {
            let i = self.focus;
            self.values[i].push('\n');
            self.dirty = true;
        } else {
            self.next_field();
        }
    }

    pub fn first_field(&mut self) {
        self.focus = 0;
        self.desc_scroll = 0;
    }

    pub fn last_field(&mut self) {
        self.focus = FIELDS.len() - 1;
        self.desc_scroll = 0;
    }

    /// `dd` — clear the focused field.
    pub fn clear_field(&mut self) {
        let i = self.focus;
        if self.focused().kind == FieldKind::Choice {
            self.values[i] = UNSET.to_string();
        } else {
            self.values[i].clear();
        }
        self.dirty = true;
        self.error = None;
    }

    pub fn enter_insert(&mut self) {
        // A choice field has nothing to type into; `i` there is almost
        // certainly a mis-key, so stay put rather than trapping the user
        // in an insert mode where no key does anything.
        if self.focused().kind != FieldKind::Choice {
            self.mode = FormMode::Insert;
        }
    }

    pub fn enter_normal(&mut self) {
        self.mode = FormMode::Normal;
        self.pending = None;
        self.command.clear();
    }

    // -------------------------------------------------------- Normal mode

    /// Handle one Normal-mode key, returning what the caller should do.
    pub fn normal_key(&mut self, c: char) -> FormAction {
        // Two-key sequences first: gg, dd, ZZ, ZQ.
        if let Some(first) = self.pending.take() {
            return match (first, c) {
                ('g', 'g') => {
                    self.first_field();
                    FormAction::None
                }
                ('d', 'd') => {
                    self.clear_field();
                    FormAction::None
                }
                ('Z', 'Z') => FormAction::Save,
                ('Z', 'Q') => FormAction::Discard,
                // An unrecognized pair is dropped rather than replayed —
                // same as vim, where `dx` does nothing surprising.
                _ => FormAction::None,
            };
        }

        match c {
            'g' | 'd' | 'Z' => {
                self.pending = Some(c);
                FormAction::None
            }
            'j' => {
                self.next_field();
                FormAction::None
            }
            'k' => {
                self.prev_field();
                FormAction::None
            }
            'G' => {
                self.last_field();
                FormAction::None
            }
            'i' | 'a' | 'A' => {
                self.enter_insert();
                FormAction::None
            }
            // h/l cycle a choice, matching left/right everywhere else.
            'h' => {
                self.cycle(-1);
                FormAction::None
            }
            'l' | ' ' => {
                self.cycle(1);
                FormAction::None
            }
            'x' => {
                self.backspace();
                FormAction::None
            }
            ':' => {
                self.mode = FormMode::Command;
                self.command.clear();
                FormAction::None
            }
            _ => FormAction::None,
        }
    }

    // ------------------------------------------------------- Command line

    pub fn command_push(&mut self, c: char) {
        self.command.push(c);
    }

    pub fn command_backspace(&mut self) -> FormAction {
        if self.command.pop().is_none() {
            // Backspacing past the colon leaves the command line, the way
            // it does in vim.
            self.enter_normal();
        }
        FormAction::None
    }

    /// Run the typed `:` command.
    pub fn command_submit(&mut self) -> FormAction {
        let cmd = self.command.trim().to_string();
        self.command.clear();
        self.mode = FormMode::Normal;
        match cmd.as_str() {
            "w" | "wq" | "x" | "wq!" | "x!" => FormAction::Save,
            "q!" => FormAction::Discard,
            "q" => FormAction::QuitIfClean,
            other => {
                self.error = Some(format!("Not a command: :{other}"));
                FormAction::None
            }
        }
    }

    /// Cycle a choice field. `delta` is +1 or -1; the list wraps, and
    /// includes UNSET so you can always get back to "no answer".
    pub fn cycle(&mut self, delta: isize) {
        let options: Vec<String> = match self.focused().id {
            FieldId::Stage => std::iter::once(UNSET.to_string())
                .chain(Stage::ALL.iter().map(|s| s.as_str().to_string()))
                .collect(),
            FieldId::Channel => std::iter::once(UNSET.to_string())
                .chain(Channel::ALL.iter().map(|c| c.as_str().to_string()))
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
        self.dirty = true;
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
            channel: choice(FieldId::Channel).and_then(|c| Channel::parse(&c)),
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
    fn the_channel_field_round_trips() {
        let mut f = filled();
        f.focus = index_of(FieldId::Channel);
        assert_eq!(f.to_entry().unwrap().channel, None, "unset stays unset");
        f.cycle(1);
        assert_eq!(f.get(FieldId::Channel), "direct");
        f.cycle(1);
        assert_eq!(f.to_entry().unwrap().channel, Some(Channel::Agency));
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
    fn opens_in_insert_and_esc_drops_to_normal() {
        let mut f = Form::new();
        assert_eq!(f.mode, FormMode::Insert);
        f.enter_normal();
        assert_eq!(f.mode, FormMode::Normal);
        f.normal_key('i');
        assert_eq!(f.mode, FormMode::Insert);
    }

    #[test]
    fn zz_writes_and_zq_discards() {
        let mut f = filled();
        f.enter_normal();
        assert_eq!(f.normal_key('Z'), FormAction::None, "first key just arms");
        assert_eq!(f.normal_key('Z'), FormAction::Save);
        assert_eq!(f.normal_key('Z'), FormAction::None);
        assert_eq!(f.normal_key('Q'), FormAction::Discard);
    }

    #[test]
    fn an_unrecognized_two_key_sequence_does_nothing() {
        let mut f = filled();
        f.enter_normal();
        f.normal_key('Z');
        // `Zx` is not a command; it must not fall through to `x`'s
        // meaning and silently delete a character.
        assert_eq!(f.normal_key('x'), FormAction::None);
        assert_eq!(f.get(FieldId::Company), "Acme Corp");
    }

    #[test]
    fn colon_commands_map_to_the_right_actions() {
        let mut f = filled();
        for (cmd, expected) in [
            ("w", FormAction::Save),
            ("wq", FormAction::Save),
            ("x", FormAction::Save),
            ("q!", FormAction::Discard),
            ("q", FormAction::QuitIfClean),
        ] {
            f.enter_normal();
            f.normal_key(':');
            assert_eq!(f.mode, FormMode::Command);
            for c in cmd.chars() {
                f.command_push(c);
            }
            assert_eq!(f.command_submit(), expected, "for :{cmd}");
            assert_eq!(f.mode, FormMode::Normal);
        }
    }

    #[test]
    fn an_unknown_colon_command_reports_rather_than_acting() {
        let mut f = filled();
        f.enter_normal();
        f.normal_key(':');
        for c in "wat".chars() {
            f.command_push(c);
        }
        assert_eq!(f.command_submit(), FormAction::None);
        assert!(f.error.unwrap().contains("Not a command: :wat"));
    }

    #[test]
    fn backspacing_past_the_colon_leaves_the_command_line() {
        let mut f = filled();
        f.enter_normal();
        f.normal_key(':');
        f.command_push('w');
        f.command_backspace();
        assert_eq!(f.mode, FormMode::Command, "still editing an empty command");
        f.command_backspace();
        assert_eq!(f.mode, FormMode::Normal, "empty backspace exits");
    }

    #[test]
    fn dirty_tracks_real_edits_only() {
        let mut f = Form::new();
        assert!(!f.dirty);
        f.enter_normal();
        f.normal_key('j'); // navigation isn't an edit
        assert!(!f.dirty);
        f.normal_key('i');
        f.insert_char('x');
        assert!(f.dirty);

        // Pre-filling from an existing row is not an edit either.
        let row = crate::model::JobRow {
            url: "manual:a/b".into(),
            company: "A".into(),
            title: "B".into(),
            ..Default::default()
        };
        assert!(!Form::from_row(&row).dirty);
    }

    #[test]
    fn dd_clears_a_field_and_resets_a_choice_to_unset() {
        let mut f = filled();
        f.set(FieldId::Stage, "seed");
        f.enter_normal();
        f.normal_key('d');
        f.normal_key('d');
        assert_eq!(f.get(FieldId::Company), "", "focus was on company");

        f.focus = index_of(FieldId::Stage);
        f.normal_key('d');
        f.normal_key('d');
        assert_eq!(f.get(FieldId::Stage), UNSET);
    }

    #[test]
    fn hl_cycles_a_choice_like_the_arrow_keys() {
        let mut f = filled();
        f.focus = index_of(FieldId::Stage);
        f.enter_normal();
        f.normal_key('l');
        assert_eq!(f.get(FieldId::Stage), "pre-seed");
        f.normal_key('h');
        assert_eq!(f.get(FieldId::Stage), UNSET);
    }

    #[test]
    fn gg_and_g_jump_to_the_first_and_last_field() {
        let mut f = filled();
        f.enter_normal();
        f.normal_key('G');
        assert_eq!(f.focus, FIELDS.len() - 1);
        f.normal_key('g');
        f.normal_key('g');
        assert_eq!(f.focus, 0);
    }

    #[test]
    fn insert_is_refused_on_a_choice_field() {
        let mut f = filled();
        f.focus = index_of(FieldId::Stage);
        f.enter_normal();
        f.normal_key('i');
        assert_eq!(
            f.mode,
            FormMode::Normal,
            "nothing to type into a choice field"
        );
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
