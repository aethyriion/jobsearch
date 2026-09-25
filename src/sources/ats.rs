//! Thin clients for each ATS's public job-board API — one board per
//! company, precise and low-noise, as opposed to the keyword search in
//! `aggregators.rs`.
//!
//! Every fetcher returns `Vec<Job>`. `description` is whatever the ATS
//! gives us (HTML or plain text) and is fetched from the SAME request as
//! the listing wherever the API supports that (Greenhouse `?content=true`,
//! Ashby and Lever include it by default), so this doesn't multiply call
//! volume. Workable's widget API may or may not include it depending on
//! the account; when it's absent the description is empty and the stack
//! check is skipped for that job, with title and location filters still
//! applying.
//!
//! SmartRecruiters is the exception: its list endpoint omits the JD, so
//! the description needs one extra request per posting. That request is
//! gated on the posting already passing title and location, for the same
//! reason the Adzuna full-JD fetch is — otherwise a company with hundreds
//! of postings becomes hundreds of extra requests for roles that would be
//! dropped on title alone anyway.

use anyhow::Result;
use serde::Deserialize;

use super::http;
use crate::config::Company;
use crate::filters::Filters;
use crate::model::{Job, StageHint};

/// Every ATS this binary can talk to. Public so `config check` and the
/// error message for an unknown `ats:` value can list them without
/// duplicating the list.
pub const SUPPORTED: [&str; 5] = [
    "greenhouse",
    "ashby",
    "workable",
    "lever",
    "smartrecruiters",
];

pub async fn fetch_company(company: &Company, filters: &Filters) -> Result<Vec<Job>> {
    let name = &company.name;
    let slug = &company.slug;
    let mut jobs = match company.ats.as_str() {
        "greenhouse" => fetch_greenhouse(name, slug).await?,
        "ashby" => fetch_ashby(name, slug).await?,
        "workable" => fetch_workable(name, slug).await?,
        "lever" => fetch_lever(name, slug).await?,
        "smartrecruiters" => fetch_smartrecruiters(name, slug, filters).await?,
        other => anyhow::bail!(
            "no fetcher for ATS type {other:?} (supported: {})",
            SUPPORTED.join(", ")
        ),
    };
    // A curated stage in companies.yaml rides along with every job from
    // that company, so startup mode works on the first run without the AI
    // step having been paid for yet.
    let hint = company.parsed_stage().map(|stage| StageHint {
        stage,
        headcount: company.headcount,
        source: crate::model::StageSource::Curated,
    });
    for job in &mut jobs {
        job.source = company.ats.clone();
        job.stage_hint = hint.clone();
    }
    Ok(jobs)
}

// ------------------------------------------------------------- greenhouse

#[derive(Deserialize)]
struct GhResponse {
    #[serde(default)]
    jobs: Vec<GhJob>,
}

#[derive(Deserialize)]
struct GhJob {
    #[serde(default)]
    title: String,
    #[serde(default)]
    location: Option<GhLocation>,
    #[serde(default)]
    absolute_url: String,
    #[serde(default)]
    first_published: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default)]
    content: Option<String>,
}

#[derive(Deserialize)]
struct GhLocation {
    #[serde(default)]
    name: String,
}

pub async fn fetch_greenhouse(company: &str, slug: &str) -> Result<Vec<Job>> {
    let url = format!("https://boards-api.greenhouse.io/v1/boards/{slug}/jobs");
    let data: GhResponse = http::get_json_with(&url, &[("content".into(), "true".into())]).await?;
    Ok(data
        .jobs
        .into_iter()
        .map(|j| Job {
            company: company.to_string(),
            title: j.title,
            location: j.location.map(|l| l.name).unwrap_or_default(),
            url: j.absolute_url,
            posted_at: j.first_published.or(j.updated_at),
            description: j.content.unwrap_or_default(),
            ..Default::default()
        })
        .collect())
}

// ------------------------------------------------------------------ ashby

#[derive(Deserialize)]
struct AshbyResponse {
    #[serde(default)]
    jobs: Vec<AshbyJob>,
}

#[derive(Deserialize)]
struct AshbyJob {
    #[serde(default)]
    title: String,
    #[serde(default)]
    location: Option<String>,
    #[serde(default, rename = "locationName")]
    location_name: Option<String>,
    #[serde(default, rename = "jobUrl")]
    job_url: Option<String>,
    #[serde(default, rename = "applyUrl")]
    apply_url: Option<String>,
    #[serde(default, rename = "publishedAt")]
    published_at: Option<String>,
    #[serde(default, rename = "descriptionPlain")]
    description_plain: Option<String>,
    #[serde(default, rename = "descriptionHtml")]
    description_html: Option<String>,
}

pub async fn fetch_ashby(company: &str, slug: &str) -> Result<Vec<Job>> {
    let url = format!("https://api.ashbyhq.com/posting-api/job-board/{slug}");
    let data: AshbyResponse = http::get_json(&url).await?;
    Ok(data
        .jobs
        .into_iter()
        .map(|j| Job {
            company: company.to_string(),
            title: j.title,
            location: j.location.or(j.location_name).unwrap_or_default(),
            url: j.job_url.or(j.apply_url).unwrap_or_default(),
            posted_at: j.published_at,
            description: j
                .description_plain
                .or(j.description_html)
                .unwrap_or_default(),
            ..Default::default()
        })
        .collect())
}

// --------------------------------------------------------------- workable

#[derive(Deserialize)]
struct WorkableResponse {
    #[serde(default)]
    jobs: Vec<WorkableJob>,
}

#[derive(Deserialize)]
struct WorkableJob {
    #[serde(default)]
    title: String,
    #[serde(default)]
    location: Option<WorkableLocation>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    shortlink: Option<String>,
    #[serde(default)]
    published_on: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Deserialize)]
struct WorkableLocation {
    #[serde(default)]
    city: Option<String>,
    #[serde(default)]
    region: Option<String>,
    #[serde(default)]
    country: Option<String>,
    #[serde(default)]
    workplace: Option<String>,
}

pub async fn fetch_workable(company: &str, slug: &str) -> Result<Vec<Job>> {
    let url = format!("https://apply.workable.com/api/v1/widget/accounts/{slug}?details=true");
    let data: WorkableResponse = http::get_json(&url).await?;
    Ok(data
        .jobs
        .into_iter()
        .map(|j| {
            let loc = j.location.unwrap_or(WorkableLocation {
                city: None,
                region: None,
                country: None,
                workplace: None,
            });
            let mut loc_str = [loc.city, loc.region, loc.country]
                .into_iter()
                .flatten()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(", ");
            if loc.workplace.as_deref() == Some("remote") {
                loc_str = if loc_str.is_empty() {
                    "Remote".to_string()
                } else {
                    format!("Remote ({loc_str})")
                };
            }
            Job {
                company: company.to_string(),
                title: j.title,
                location: loc_str,
                url: j.url.or(j.shortlink).unwrap_or_default(),
                posted_at: j.published_on.or(j.created_at),
                description: j.description.unwrap_or_default(),
                ..Default::default()
            }
        })
        .collect())
}

// ------------------------------------------------------------------ lever

#[derive(Deserialize)]
struct LeverJob {
    #[serde(default)]
    text: String,
    #[serde(default)]
    categories: Option<LeverCategories>,
    #[serde(default, rename = "hostedUrl")]
    hosted_url: String,
    #[serde(default, rename = "createdAt")]
    created_at: Option<i64>,
    #[serde(default, rename = "descriptionPlain")]
    description_plain: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Deserialize)]
struct LeverCategories {
    #[serde(default)]
    location: Option<String>,
    #[serde(default, rename = "allLocations")]
    all_locations: Option<Vec<String>>,
}

pub async fn fetch_lever(company: &str, slug: &str) -> Result<Vec<Job>> {
    let url = format!("https://api.lever.co/v0/postings/{slug}?mode=json");
    let data: Vec<LeverJob> = http::get_json(&url).await?;
    Ok(data
        .into_iter()
        .map(|j| {
            let cats = j.categories.unwrap_or(LeverCategories {
                location: None,
                all_locations: None,
            });
            let location = match cats.all_locations {
                Some(all) if !all.is_empty() => all.join(", "),
                _ => cats.location.unwrap_or_default(),
            };
            Job {
                company: company.to_string(),
                title: j.text,
                location,
                url: j.hosted_url,
                // Lever gives epoch milliseconds; everything downstream
                // treats posted_at as an opaque display string, so render
                // it as an ISO date here rather than leaving a bare
                // 13-digit number in the UI.
                posted_at: j.created_at.and_then(epoch_millis_to_iso),
                description: j.description_plain.or(j.description).unwrap_or_default(),
                ..Default::default()
            }
        })
        .collect())
}

fn epoch_millis_to_iso(ms: i64) -> Option<String> {
    chrono::DateTime::from_timestamp_millis(ms).map(|dt| dt.format("%Y-%m-%d").to_string())
}

// -------------------------------------------------------- smartrecruiters

#[derive(Deserialize)]
struct SrResponse {
    #[serde(default)]
    content: Vec<SrPosting>,
}

#[derive(Deserialize)]
struct SrPosting {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: String,
    #[serde(default)]
    location: Option<SrLocation>,
    #[serde(default, rename = "applyUrl")]
    apply_url: Option<String>,
    #[serde(default, rename = "ref")]
    reference: Option<String>,
    #[serde(default, rename = "releasedDate")]
    released_date: Option<String>,
}

#[derive(Deserialize)]
struct SrLocation {
    #[serde(default)]
    city: Option<String>,
    #[serde(default)]
    region: Option<String>,
    #[serde(default)]
    country: Option<String>,
    #[serde(default)]
    remote: Option<bool>,
}

pub async fn fetch_smartrecruiters(
    company: &str,
    slug: &str,
    filters: &Filters,
) -> Result<Vec<Job>> {
    let mut jobs = Vec::new();
    let mut offset = 0usize;
    let limit = 100usize;
    loop {
        let url = format!("https://api.smartrecruiters.com/v1/companies/{slug}/postings");
        let data: SrResponse = http::get_json_with(
            &url,
            &[
                ("limit".into(), limit.to_string()),
                ("offset".into(), offset.to_string()),
            ],
        )
        .await?;
        let page_len = data.content.len();
        if page_len == 0 {
            break;
        }

        for p in data.content {
            let loc = p.location.unwrap_or(SrLocation {
                city: None,
                region: None,
                country: None,
                remote: None,
            });
            let mut loc_str = [loc.city, loc.region, loc.country]
                .into_iter()
                .flatten()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(", ");
            if loc.remote == Some(true) {
                loc_str = if loc_str.is_empty() {
                    "Remote".to_string()
                } else {
                    format!("Remote ({loc_str})")
                };
            }

            let posting_id = p.id.unwrap_or_default();
            let job_url = p
                .apply_url
                .or(p.reference)
                .filter(|u| !u.is_empty())
                .or_else(|| {
                    (!posting_id.is_empty())
                        .then(|| format!("https://jobs.smartrecruiters.com/{slug}/{posting_id}"))
                });
            // No applyUrl, ref or id to build any identifier from — skip
            // rather than store an empty URL, which would make dedup treat
            // every subsequent url-less posting as a duplicate of the
            // first.
            let Some(job_url) = job_url else { continue };

            let mut description = String::new();
            if !posting_id.is_empty()
                && filters.title_is_relevant(&p.name)
                && filters.location_is_allowed(&loc_str)
            {
                description = fetch_smartrecruiters_description(slug, &posting_id).await;
            }

            jobs.push(Job {
                company: company.to_string(),
                title: p.name,
                location: loc_str,
                url: job_url,
                posted_at: p.released_date,
                description,
                ..Default::default()
            });
        }

        if page_len < limit {
            break;
        }
        offset += limit;
    }
    Ok(jobs)
}

/// Best-effort: any failure (private board, 404, timeout) means no
/// description, not a failed run — the filters treat an empty description
/// as "unknown, don't reject on stack alone".
async fn fetch_smartrecruiters_description(slug: &str, posting_id: &str) -> String {
    #[derive(Deserialize)]
    struct Detail {
        #[serde(default, rename = "jobAd")]
        job_ad: Option<JobAd>,
    }
    #[derive(Deserialize)]
    struct JobAd {
        #[serde(default)]
        sections: Option<serde_json::Value>,
    }

    let url = format!("https://api.smartrecruiters.com/v1/companies/{slug}/postings/{posting_id}");
    let Ok(detail) = http::get_json::<Detail>(&url).await else {
        return String::new();
    };
    let Some(sections) = detail.job_ad.and_then(|a| a.sections) else {
        return String::new();
    };
    ["jobDescription", "qualifications", "additionalInformation"]
        .iter()
        .filter_map(|key| {
            sections
                .get(key)
                .and_then(|s| s.get("text"))
                .and_then(|t| t.as_str())
                .filter(|t| !t.is_empty())
                .map(str::to_string)
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}
