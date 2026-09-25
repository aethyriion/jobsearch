//! Broad job aggregators: keyword + location search across many employers
//! at once, as opposed to `ats.rs`'s one-board-per-company. They catch
//! companies you haven't identified yet, at the cost of far more noise —
//! which is exactly what `filters.rs` is for.

use anyhow::{anyhow, Result};
use regex::Regex;
use serde::Deserialize;
use std::sync::OnceLock;

use super::http;
use crate::config::Aggregator;
use crate::filters::Filters;
use crate::model::Job;

/// A company discovered mid-run: a full-JD fetch resolved an aggregator's
/// redirect to a real Greenhouse/Lever board. Getting one into
/// companies.yaml means every future run pulls that company's WHOLE board
/// directly rather than whatever the aggregator happened to surface.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DiscoveredCompany {
    pub name: String,
    pub ats: String,
    pub slug: String,
}

pub async fn fetch_aggregator(
    agg: &Aggregator,
    filters: &Filters,
    discovered: &mut Vec<DiscoveredCompany>,
) -> Result<Vec<Job>> {
    let mut jobs = match agg.kind.as_str() {
        "adzuna" => fetch_adzuna(agg, filters, discovered).await?,
        "remotive" => fetch_remotive(agg).await?,
        "hn" => super::hn::fetch(agg, filters).await?,
        other => anyhow::bail!(
            "no fetcher for aggregator type {other:?} \
             (supported: adzuna, remotive, hn; 'yc' runs via `jobsearch discover yc`)"
        ),
    };
    for job in &mut jobs {
        if job.source.is_empty() {
            job.source = agg.kind.clone();
        }
    }
    Ok(jobs)
}

// ----------------------------------------------------------------- adzuna

#[derive(Deserialize)]
struct AdzunaResponse {
    #[serde(default)]
    results: Vec<AdzunaJob>,
}

#[derive(Deserialize)]
struct AdzunaJob {
    #[serde(default)]
    title: String,
    #[serde(default)]
    location: Option<AdzunaLocation>,
    #[serde(default)]
    company: Option<AdzunaCompany>,
    #[serde(default)]
    redirect_url: String,
    #[serde(default)]
    created: Option<String>,
    #[serde(default)]
    description: String,
}

#[derive(Deserialize)]
struct AdzunaLocation {
    #[serde(default)]
    display_name: String,
}

#[derive(Deserialize)]
struct AdzunaCompany {
    #[serde(default)]
    display_name: String,
}

/// Adzuna paginates, and the free tier's ranking means good matches can be
/// several pages deep — so walk up to `max_pages`, stopping early when a
/// page comes back short (which signals the end of the result set).
///
/// Two of the params in aggregators.yaml are pipeline-level rather than
/// API-level and are consumed here instead of being sent: `max_pages`, and
/// `country` — Adzuna's country code is part of the URL *path*, not a
/// query parameter.
async fn fetch_adzuna(
    agg: &Aggregator,
    filters: &Filters,
    discovered: &mut Vec<DiscoveredCompany>,
) -> Result<Vec<Job>> {
    let app_id = std::env::var("ADZUNA_APP_ID").unwrap_or_default();
    let app_key = std::env::var("ADZUNA_APP_KEY").unwrap_or_default();
    if app_id.is_empty() || app_key.is_empty() {
        return Err(anyhow!(
            "ADZUNA_APP_ID / ADZUNA_APP_KEY not set — register free at \
             https://developer.adzuna.com, or set `enabled: false` on this entry"
        ));
    }

    let max_pages = agg.param_u64("max_pages").unwrap_or(5);
    let country = agg.param_str("country").unwrap_or_else(|| "us".into());
    let results_per_page = agg.param_u64("results_per_page").unwrap_or(50) as usize;
    let passthrough = agg.query_params(&["max_pages", "country"]);

    let mut jobs = Vec::new();
    for page in 1..=max_pages {
        let url = format!("https://api.adzuna.com/v1/api/jobs/{country}/search/{page}");
        let mut query = vec![
            ("app_id".to_string(), app_id.clone()),
            ("app_key".to_string(), app_key.clone()),
            ("content-type".to_string(), "application/json".to_string()),
        ];
        query.extend(passthrough.iter().cloned());

        let data: AdzunaResponse = http::get_json_with(&url, &query).await?;
        let page_len = data.results.len();
        if page_len == 0 {
            break;
        }

        for j in data.results {
            let location = j.location.map(|l| l.display_name).unwrap_or_default();
            let company = j
                .company
                .map(|c| c.display_name)
                .filter(|c| !c.is_empty())
                .unwrap_or_else(|| "Unknown".to_string());
            let snippet = j.description;
            let mut description = snippet.clone();

            // Only worth an extra request for jobs that already look like
            // real candidates AND whose snippet is actually thin. Most of
            // Adzuna's results get dropped on title/location alone, so
            // gating here is what keeps this from becoming thousands of
            // extra HTTP requests per run.
            if filters.title_is_relevant(&j.title)
                && filters.location_is_allowed(&location)
                && filters.looks_truncated(&snippet)
            {
                let full = fetch_full_description(&j.redirect_url).await;
                if let Some(text) = &full.description {
                    if text.len() > snippet.len() {
                        description = text.clone();
                    }
                }
                if let (Some(ats), Some(slug)) = (full.ats, full.slug) {
                    discovered.push(DiscoveredCompany {
                        name: company.clone(),
                        ats,
                        slug,
                    });
                }
            }

            jobs.push(Job {
                company,
                title: j.title,
                location,
                url: j.redirect_url,
                posted_at: j.created,
                description,
                ..Default::default()
            });
        }

        if page_len < results_per_page {
            break;
        }
    }
    Ok(jobs)
}

#[derive(Debug, Default)]
struct FullDescription {
    description: Option<String>,
    ats: Option<String>,
    slug: Option<String>,
}

fn greenhouse_url_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?:job-boards|boards)\.greenhouse\.io/([^/]+)/jobs/(\d+)").unwrap()
    })
}

fn lever_url_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"jobs\.lever\.co/([^/]+)/([0-9a-f-]{36})").unwrap())
}

/// Follow an aggregator's redirect to the real posting and pull the full
/// JD. Adzuna's search API only ever returns a short teaser — there is no
/// "give me the full text" parameter — so this is the only way to get a
/// real description.
///
/// Prefers the source ATS's own single-job JSON endpoint when the redirect
/// lands on a board we recognize (cheap, structured, already parsed
/// elsewhere in this crate), and falls back to scraping whatever HTML the
/// final page returns.
///
/// `ats`/`slug` are set ONLY when a recognized ATS's API call actually
/// succeeded. That distinction is the whole point: a slug *parsed off a
/// URL* is a guess, and companies.yaml has scar tissue from two of those
/// burning real API calls on 404s. A slug that came back from a successful
/// request is verified.
///
/// Known limitation: when the aggregator is the terminal host (the
/// redirect stays on adzuna.* rather than going on to a real ATS), the
/// page is a client-rendered shell whose content arrives via JavaScript —
/// no plain HTTP client can ever see it, whatever headers it sends. Those
/// jobs fall through to the short snippet, and the AI step is told the
/// description is partial so a thin snippet doesn't quietly tank the
/// score.
async fn fetch_full_description(redirect_url: &str) -> FullDescription {
    if redirect_url.is_empty() {
        return FullDescription::default();
    }
    let Ok((final_url, body)) =
        http::get_text_following_redirects(redirect_url, http::FULL_JD_TIMEOUT).await
    else {
        return FullDescription::default();
    };

    if let Some(caps) = greenhouse_url_re().captures(&final_url) {
        let (slug, job_id) = (&caps[1], &caps[2]);
        let url = format!("https://boards-api.greenhouse.io/v1/boards/{slug}/jobs/{job_id}");
        if let Ok(v) =
            http::get_json_with::<serde_json::Value>(&url, &[("content".into(), "true".into())])
                .await
        {
            if let Some(content) = v.get("content").and_then(|c| c.as_str()) {
                if !content.is_empty() {
                    return FullDescription {
                        description: Some(content.to_string()),
                        ats: Some("greenhouse".into()),
                        slug: Some(slug.to_string()),
                    };
                }
            }
        }
    }

    if let Some(caps) = lever_url_re().captures(&final_url) {
        let (slug, posting_id) = (&caps[1], &caps[2]);
        let url = format!("https://api.lever.co/v0/postings/{slug}/{posting_id}?mode=json");
        if let Ok(v) = http::get_json::<serde_json::Value>(&url).await {
            let content = v
                .get("descriptionPlain")
                .or_else(|| v.get("description"))
                .and_then(|c| c.as_str());
            if let Some(content) = content.filter(|c| !c.is_empty()) {
                return FullDescription {
                    description: Some(content.to_string()),
                    ats: Some("lever".into()),
                    slug: Some(slug.to_string()),
                };
            }
        }
    }

    let text = http::html_page_to_text(&body);
    FullDescription {
        description: (!text.is_empty()).then_some(text),
        ..Default::default()
    }
}

// --------------------------------------------------------------- remotive

#[derive(Deserialize)]
struct RemotiveResponse {
    #[serde(default)]
    jobs: Vec<RemotiveJob>,
}

#[derive(Deserialize)]
struct RemotiveJob {
    #[serde(default)]
    company_name: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    candidate_required_location: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    publication_date: Option<String>,
    #[serde(default)]
    description: String,
}

async fn fetch_remotive(agg: &Aggregator) -> Result<Vec<Job>> {
    let query = agg.query_params(&[]);
    let data: RemotiveResponse =
        http::get_json_with("https://remotive.com/api/remote-jobs", &query).await?;
    Ok(data
        .jobs
        .into_iter()
        .map(|j| Job {
            company: if j.company_name.is_empty() {
                "Unknown".to_string()
            } else {
                j.company_name
            },
            title: j.title,
            location: j.candidate_required_location,
            url: j.url,
            posted_at: j.publication_date,
            description: j.description,
            ..Default::default()
        })
        .collect())
}
