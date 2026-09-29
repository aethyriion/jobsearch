//! Hacker News "Ask HN: Who is hiring?" — a startup-native job source.
//!
//! Why this is here: the monthly thread is overwhelmingly small companies
//! posting for themselves, very often a founder. That makes it one of the
//! few free, no-auth, no-scraping-fight sources where the *median* posting
//! is the kind of role startup mode exists to surface — the opposite of a
//! general aggregator, where startups are a thin slice under a mountain of
//! staffing-agency noise.
//!
//! It runs on the public Algolia HN API (`hn.algolia.com/api/v1`), which
//! needs no key and is the same API the HN search box uses.
//!
//! The convention posters follow is a pipe-delimited first line:
//!
//!     Company | Role | Location | REMOTE | Full-time | $range | url
//!
//! Adherence is good but not universal, so `parse_posting` treats the
//! convention as a hint and degrades rather than dropping a post it can't
//! parse cleanly: the full comment text always becomes the description, so
//! even a post this parser reads badly still reaches the AI step with
//! everything a human would have seen.

use anyhow::Result;
use serde::Deserialize;

use super::http;
use crate::config::Aggregator;
use crate::filters::Filters;
use crate::model::{Job, Stage, StageHint, StageSource};

const API: &str = "https://hn.algolia.com/api/v1";

#[derive(Deserialize)]
struct SearchResponse {
    #[serde(default)]
    hits: Vec<Hit>,
    #[serde(default)]
    nb_pages: Option<u64>,
}

#[derive(Deserialize)]
struct Hit {
    #[serde(
        default,
        rename = "objectID",
        deserialize_with = "http::null_to_default"
    )]
    object_id: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    comment_text: Option<String>,
    #[serde(default)]
    parent_id: Option<u64>,
    #[serde(default)]
    created_at: Option<String>,
}

/// Fetch the most recent N "Who is hiring?" threads and turn their
/// top-level comments into jobs.
///
/// `months` (default 1) is how many threads back to read. One month is the
/// right default for a recurring run — anything older has mostly been
/// filled, and dedup means a re-run of the same thread costs nothing.
pub async fn fetch(agg: &Aggregator, filters: &Filters) -> Result<Vec<Job>> {
    let months = agg.param_u64("months").unwrap_or(1).clamp(1, 12) as usize;
    let threads = recent_hiring_threads(months).await?;

    let mut jobs = Vec::new();
    for (story_id, thread_title) in threads {
        jobs.extend(fetch_thread(story_id, &thread_title, filters).await?);
    }
    Ok(jobs)
}

/// The `whoishiring` account posts all three monthly threads at the same
/// minute, so filter to the hiring one by title rather than taking the
/// newest — otherwise you get "Who wants to be hired?", which is
/// candidates, not jobs.
async fn recent_hiring_threads(months: usize) -> Result<Vec<(u64, String)>> {
    let url = format!(
        "{API}/search_by_date?tags=story,author_whoishiring&hitsPerPage={}",
        months * 3 + 3
    );
    let resp: SearchResponse = http::get_json(&url).await?;
    Ok(resp
        .hits
        .into_iter()
        .filter_map(|h| {
            let title = h.title?;
            let lower = title.to_lowercase();
            if !lower.contains("who is hiring") {
                return None;
            }
            Some((h.object_id.parse().ok()?, title))
        })
        .take(months)
        .collect())
}

async fn fetch_thread(story_id: u64, thread_title: &str, filters: &Filters) -> Result<Vec<Job>> {
    let mut jobs = Vec::new();
    let mut page = 0u64;
    loop {
        let url = format!("{API}/search?tags=comment,story_{story_id}&hitsPerPage=200&page={page}");
        let resp: SearchResponse = http::get_json(&url).await?;
        if resp.hits.is_empty() {
            break;
        }
        for hit in &resp.hits {
            // The tag query returns every descendant of the story, so
            // replies come back too. Only direct children are postings —
            // without this check, every "we're hiring too!" reply and
            // every argument in the thread becomes a job.
            if hit.parent_id != Some(story_id) {
                continue;
            }
            let Some(text) = hit.comment_text.as_deref() else {
                continue;
            };
            if let Some(job) = parse_posting(text, hit, story_id, thread_title, filters) {
                jobs.push(job);
            }
        }
        page += 1;
        if page >= resp.nb_pages.unwrap_or(1) || page > 20 {
            break;
        }
    }
    Ok(jobs)
}

fn parse_posting(
    comment_html: &str,
    hit: &Hit,
    story_id: u64,
    thread_title: &str,
    filters: &Filters,
) -> Option<Job> {
    // HN comments are HTML with <p> separators; the convention line is the
    // first paragraph.
    let text = crate::filters::strip_html(&comment_html.replace("<p>", "\n"));
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let first_line = text.lines().find(|l| !l.trim().is_empty())?.trim();

    let fields: Vec<&str> = first_line.split('|').map(str::trim).collect();
    // Fewer than two fields means the poster didn't follow the convention
    // at all. Those are mostly meta-commentary ("this is who is hiring,
    // you're not hiring") rather than postings, and guessing a company and
    // title out of free prose would manufacture noise.
    if fields.len() < 2 {
        return None;
    }

    let company = fields[0].trim();
    let title = fields[1].trim();
    if company.is_empty() || title.is_empty() || company.len() > 80 {
        return None;
    }

    // Location is whichever remaining field looks like a place or a remote
    // marker — posters put REMOTE in position 3, 4 or 5 about equally.
    let location = fields[2..]
        .iter()
        .find(|f| looks_like_location(f))
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            if text.to_lowercase().contains("remote") {
                "Remote".to_string()
            } else {
                String::new()
            }
        });

    // The apply link if the poster gave one, else a permalink to the
    // comment itself. A stable unique URL matters beyond convenience: it
    // is half the dedup key, so a posting with no link still has to get a
    // URL that's its own.
    let url = first_url_in(comment_html)
        .unwrap_or_else(|| format!("https://news.ycombinator.com/item?id={}", hit.object_id));

    let job = Job {
        company: company.to_string(),
        title: title.to_string(),
        location,
        url,
        posted_at: hit.created_at.clone(),
        description: format!(
            "Posted by {} in \"{}\" (Hacker News).\n\n{}",
            hit.author.as_deref().unwrap_or("unknown"),
            thread_title,
            text
        ),
        source: "hn".to_string(),
        // Every posting in this thread is a company hiring directly, and
        // the thread skews heavily to small companies — but "small" is not
        // a stage, and claiming one we haven't verified would put wrong
        // data behind the startup filter. Leave the stage to the AI step,
        // which reads the full post (headcount, funding and "founding
        // engineer" language are usually right there in the text).
        stage_hint: None,
    };

    // HN threads are cross-discipline (designers, sales, ops), and unlike
    // an ATS board there's no structured department to filter on — so
    // apply the title filter here rather than storing thousands of rows
    // that exist only to be dropped downstream.
    if !filters.title_is_relevant(&job.title) {
        return None;
    }
    let _ = story_id;
    Some(job)
}

fn looks_like_location(field: &str) -> bool {
    let f = field.to_lowercase();
    const MARKERS: [&str; 10] = [
        "remote",
        "onsite",
        "on-site",
        "hybrid",
        "usa",
        "us,",
        "united states",
        "sf",
        "nyc",
        "office",
    ];
    if MARKERS.iter().any(|m| f.contains(m)) {
        return true;
    }
    // "City, ST" / "City, Country" — a comma with letters either side, and
    // short enough not to be a sentence.
    field.contains(',') && field.len() < 60 && field.chars().any(char::is_alphabetic)
}

fn first_url_in(html: &str) -> Option<String> {
    // HN renders links as <a href="...">, so read the href rather than
    // regexing the visible text — the visible text is often truncated with
    // an ellipsis and would produce a dead link.
    let start = html.find("href=\"")? + 6;
    let rest = &html[start..];
    let end = rest.find('"')?;
    let url = html_escape::decode_html_entities(&rest[..end]).into_owned();
    url.starts_with("http").then_some(url)
}

/// YC companies get a stage from the directory; this is the fallback for
/// everyone else the AI step hasn't reached yet. Unused today but kept
/// deliberately: it is the hook for a future "posting mentions Series A"
/// heuristic, and is covered by tests so it can't rot silently.
#[allow(dead_code)]
pub fn stage_hint_from_text(text: &str) -> Option<StageHint> {
    let lower = text.to_lowercase();
    let stage = if lower.contains("pre-seed") || lower.contains("preseed") {
        Stage::PreSeed
    } else if lower.contains("seed-stage")
        || lower.contains("seed stage")
        || lower.contains("just raised our seed")
    {
        Stage::Seed
    } else if lower.contains("series a") {
        Stage::SeriesA
    } else if lower.contains("series b") {
        Stage::SeriesB
    } else if lower.contains("series c") || lower.contains("series d") {
        Stage::SeriesC
    } else {
        return None;
    };
    Some(StageHint {
        stage,
        headcount: None,
        source: StageSource::Ai,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{FiltersConfig, SeniorityPolicyConfig, StartupPolicyConfig};

    fn filters() -> Filters {
        Filters::compile(&FiltersConfig {
            hard_exclusion_keywords: vec!["intern".into()],
            priority_title_keywords: vec!["software engineer".into()],
            title_allow_keywords: vec!["engineer".into(), "developer".into()],
            exclusion_keywords: vec!["sales".into()],
            location_allow_patterns: vec![".*".into()],
            stack_dealbreakers: vec![r"\bc#\b".into()],
            stack_core: vec![r"\bgo\b".into()],
            truncated_description_min_chars: 400,
            startup: StartupPolicyConfig::default(),
            seniority: SeniorityPolicyConfig::default(),
            staleness: Default::default(),
            agency_text_patterns: vec![],
            agency_company_patterns: vec![],
        })
        .unwrap()
    }

    fn hit(id: &str, parent: u64) -> Hit {
        Hit {
            object_id: id.into(),
            title: None,
            author: Some("founder".into()),
            comment_text: None,
            parent_id: Some(parent),
            created_at: Some("2026-09-01T15:01:17Z".into()),
        }
    }

    #[test]
    fn parses_the_conventional_pipe_format() {
        let html = "<p>Pango (YC S26) | Founding Software Engineer | Remote (US) | Full time | \
                    <a href=\"https://pango.dev/jobs\">apply</a></p>We are building things.";
        let job = parse_posting(
            html,
            &hit("1", 9),
            9,
            "Who is hiring? (September 2026)",
            &filters(),
        )
        .expect("should parse");
        assert_eq!(job.company, "Pango (YC S26)");
        assert_eq!(job.title, "Founding Software Engineer");
        assert_eq!(job.location, "Remote (US)");
        assert_eq!(job.url, "https://pango.dev/jobs");
        assert!(job.description.contains("We are building things"));
    }

    #[test]
    fn falls_back_to_a_comment_permalink_when_no_link_is_given() {
        let html = "<p>Acme | Platform Engineer | Remote</p>";
        let job = parse_posting(html, &hit("42", 9), 9, "t", &filters()).unwrap();
        assert_eq!(job.url, "https://news.ycombinator.com/item?id=42");
    }

    #[test]
    fn rejects_meta_commentary_that_is_not_a_posting() {
        let html = "<p>This is &quot;Who is hiring&quot;... you&#x27;re not hiring.</p>";
        assert!(parse_posting(html, &hit("1", 9), 9, "t", &filters()).is_none());
    }

    #[test]
    fn applies_the_title_filter_so_non_engineering_posts_are_dropped() {
        let html = "<p>Acme | Head of Sales | Remote</p>";
        assert!(parse_posting(html, &hit("1", 9), 9, "t", &filters()).is_none());
    }

    #[test]
    fn detects_stage_language_in_prose() {
        assert_eq!(
            stage_hint_from_text("We just raised our Series A and are 20 people.").map(|h| h.stage),
            Some(Stage::SeriesA)
        );
        assert_eq!(stage_hint_from_text("We are a profitable company."), None);
    }
}
