//! Appending newly-resolved companies to `companies.yaml`.
//!
//! Getting a company into that file means every future run fetches its
//! whole board directly — every open role, not just whatever an aggregator
//! or directory happened to surface — for free.
//!
//! This deliberately does NOT round-trip the file through a YAML
//! serializer. Doing so would silently drop every hand-written comment:
//! the `ats: one of...` header, the per-company provenance notes, the
//! section markers. Instead it only ever APPENDS a commented block of
//! plain text, leaving everything already in the file untouched.

use anyhow::{Context, Result};
use std::collections::HashSet;
use std::io::Write;
use std::path::Path;

use crate::config::{load_companies, Company};
use crate::model::Stage;

/// A company to append, with whatever stage metadata the discovering
/// source knew.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewCompany {
    pub name: String,
    pub ats: String,
    pub slug: String,
    pub stage: Option<Stage>,
    pub headcount: Option<i64>,
    /// Free text written as a trailing comment — where this came from, so
    /// a future reader of the file knows whether to trust the stage.
    pub note: Option<String>,
}

/// `(ats, slug)` pairs already tracked, so we never suggest a duplicate.
pub fn known_keys(path: impl AsRef<Path>) -> Result<HashSet<(String, String)>> {
    let companies: Vec<Company> = load_companies(path)?;
    Ok(companies.into_iter().map(|c| (c.ats, c.slug)).collect())
}

/// Append whatever in `discovered` isn't already tracked, returning
/// exactly what got written (empty on the common case of nothing new).
///
/// Dedups against both the file and the batch itself — the same company
/// routinely shows up several times in one run, once per posting.
pub fn append_new_companies(
    path: impl AsRef<Path>,
    discovered: &[NewCompany],
    source_note: &str,
) -> Result<Vec<NewCompany>> {
    let path = path.as_ref();
    let known = known_keys(path)?;
    let mut seen = HashSet::new();
    let mut to_add = Vec::new();
    for c in discovered {
        let key = (c.ats.clone(), c.slug.clone());
        if known.contains(&key) || !seen.insert(key) {
            continue;
        }
        to_add.push(c.clone());
    }
    if to_add.is_empty() {
        return Ok(Vec::new());
    }

    let today = chrono::Local::now().format("%Y-%m-%d");
    let mut block = format!("\n  # Auto-discovered {today} via {source_note}\n");
    for c in &to_add {
        block.push_str(&format!("  - name: {}\n", yaml_quote(&c.name)));
        block.push_str(&format!("    ats: {}\n", c.ats));
        block.push_str(&format!("    slug: {}\n", c.slug));
        if let Some(stage) = c.stage {
            block.push_str(&format!("    stage: {}\n", stage.as_str()));
        }
        if let Some(headcount) = c.headcount {
            block.push_str(&format!("    headcount: {headcount}\n"));
        }
        if let Some(note) = &c.note {
            block.push_str(&format!("    # {}\n", note.replace('\n', " ")));
        }
    }

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .with_context(|| format!("appending to {}", path.display()))?;
    file.write_all(block.as_bytes())
        .with_context(|| format!("writing to {}", path.display()))?;
    Ok(to_add)
}

/// Quote a value the way a YAML emitter would, so a company name with a
/// colon or a leading special character ("Warner Music Group: Canada")
/// can't corrupt the file structure.
fn yaml_quote(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn temp_companies(body: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("jobsearch-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!(
            "companies-{}.yaml",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, body).unwrap();
        path
    }

    fn read(path: &std::path::Path) -> String {
        let mut s = String::new();
        std::fs::File::open(path)
            .unwrap()
            .read_to_string(&mut s)
            .unwrap();
        s
    }

    fn company(name: &str, slug: &str) -> NewCompany {
        NewCompany {
            name: name.into(),
            ats: "greenhouse".into(),
            slug: slug.into(),
            stage: Some(Stage::SeriesA),
            headcount: Some(30),
            note: Some("YC W24".into()),
        }
    }

    #[test]
    fn appends_without_touching_existing_comments() {
        let path = temp_companies(
            "# A hand-written header comment\ncompanies:\n  - name: Existing\n    ats: lever\n    slug: existing\n",
        );
        let added = append_new_companies(&path, &[company("Acme", "acme")], "test").unwrap();
        assert_eq!(added.len(), 1);
        let text = read(&path);
        assert!(
            text.contains("# A hand-written header comment"),
            "must preserve comments"
        );
        assert!(text.contains("slug: acme"));
        assert!(text.contains("stage: series-a"));
        assert!(text.contains("headcount: 30"));
        // And the result must still parse.
        let parsed = load_companies(&path).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[1].parsed_stage(), Some(Stage::SeriesA));
    }

    #[test]
    fn skips_companies_already_in_the_file() {
        let path =
            temp_companies("companies:\n  - name: Acme\n    ats: greenhouse\n    slug: acme\n");
        let added = append_new_companies(&path, &[company("Acme", "acme")], "test").unwrap();
        assert!(added.is_empty());
    }

    #[test]
    fn dedups_within_one_batch() {
        let path = temp_companies("companies:\n  - name: X\n    ats: lever\n    slug: x\n");
        let added = append_new_companies(
            &path,
            &[company("Acme", "acme"), company("Acme", "acme")],
            "test",
        )
        .unwrap();
        assert_eq!(added.len(), 1);
    }

    #[test]
    fn quotes_names_that_would_break_the_yaml() {
        // A block sequence, not `companies: []` — appending block items to
        // a flow sequence produces invalid YAML, and the real file is
        // always a block sequence.
        let path = temp_companies("companies:\n  - name: Seed\n    ats: lever\n    slug: seed\n");
        let mut c = company("Warner: Canada", "warner");
        c.stage = None;
        c.headcount = None;
        c.note = None;
        append_new_companies(&path, &[c], "test").unwrap();
        let parsed = load_companies(&path).unwrap();
        assert_eq!(parsed[1].name, "Warner: Canada");
    }
}
