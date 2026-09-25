//! Stage 2: AI evaluation via the Anthropic Messages API.
//!
//! Reads every job that passed the deterministic filters but hasn't been
//! scored yet, sends it to Claude alongside your profile, and asks for a
//! FUNCTIONAL FIT judgment — not a title match. Results go into the
//! `ai_evaluations` table and the scored CSV.
//!
//! This is the ONLY part of the pipeline that costs money. Everything
//! upstream is free, which is the whole point of filtering deterministically
//! first: by the time a job reaches here it has already passed title,
//! location and stack screening, so the scored volume is small.
//!
//! It also does double duty as the third source of startup signal. The
//! same call that scores fit is asked to place the company on the stage
//! axis, because the model is already reading the full JD — where funding
//! history, team size and "founding engineer" language usually sit in
//! plain text. That covers the long tail of aggregator- and HN-sourced
//! companies that will never be hand-curated and aren't in the YC
//! directory. A curated or YC-sourced stage always wins over this one
//! (see `Store::upsert_company_stage`).
//!
//! Uses the REST API directly rather than an SDK because Anthropic
//! publishes no official Rust SDK; the request shape below is the
//! documented `POST /v1/messages` contract.

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::model::{Evaluation, JobRow, Stage};

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2023-06-01";

/// Haiku is the default because this is a bulk classifier: it runs over
/// every candidate a fetch produced, and the task — judging fit against a
/// profile that's right there in the prompt — is one the small model does
/// well. Override with ANTHROPIC_MODEL for a run where you want a stronger
/// read (`claude-sonnet-5`, `claude-opus-5`).
pub const DEFAULT_MODEL: &str = "claude-haiku-4-5";

pub fn model_from_env() -> String {
    std::env::var("ANTHROPIC_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string())
}

const SYSTEM_PROMPT: &str = "You are evaluating job postings for FUNCTIONAL FIT against a \
candidate's real experience — not title matching, not keyword matching. The candidate's profile \
is organized by competency (what they've actually done), not by job title, specifically so you \
judge whether their demonstrated capabilities transfer to this role's actual responsibilities.

Be honest and specific, not diplomatic. A generic \"great candidate!\" evaluation is useless — the \
candidate needs real signal on whether to spend an application on this. If the role is a stretch, \
say so and say why. If there's a real gap, name it precisely rather than softening it. Cite \
specific evidence from their profile when claiming a strength transfers; don't just assert \
seniority-level fit in the abstract.

This candidate is specifically targeting startups, so the company_stage assessment matters as much \
as the score. Judge stage from evidence in the posting — funding mentioned, team size, how the \
company describes itself, whether the role is titled \"founding\" anything, equity in the \
compensation — and set stage_confidence honestly low when the posting gives you little to go on. \
Do not guess a stage from the company's name alone.";

/// Field order is load-bearing, not documentation. The model tends to emit
/// tool JSON in declaration order, and with `max_tokens` capped, a run of
/// long free-text fields can exhaust the budget before later fields are
/// written — which caused a real missing-`recommendation` failure in the
/// Python version this ports. Putting the two short, decision-critical
/// fields first means they are very unlikely to be the ones lost to
/// truncation even if a long-text field still gets cut off.
fn evaluation_schema() -> Value {
    json!({
        "name": "submit_evaluation",
        "description": "Submit a structured fit evaluation for this job posting.",
        "input_schema": {
            "type": "object",
            "properties": {
                "match_score": {
                    "type": "integer", "minimum": 0, "maximum": 100,
                    "description": "Overall functional fit score, 0-100."
                },
                "recommendation": {
                    "type": "string", "enum": ["apply", "consider", "skip"],
                    "description": "apply = strong functional fit, worth the effort. consider = plausible but real gaps/risk. skip = not a genuine fit despite passing keyword filters."
                },
                "company_stage": {
                    "type": "string",
                    "enum": ["pre-seed", "seed", "series-a", "series-b", "series-c", "growth", "public", "big-tech", "unknown"],
                    "description": "Company maturity, judged from evidence in the posting. Use 'unknown' when the posting genuinely gives you nothing to judge from — that is more useful than a guess."
                },
                "company_headcount": {
                    "type": "integer", "minimum": 0,
                    "description": "Approximate engineering+total headcount if the posting states or strongly implies one. Omit if it doesn't."
                },
                "stage_confidence": {
                    "type": "integer", "minimum": 0, "maximum": 100,
                    "description": "How confident you are in company_stage, 0-100. Low is fine and expected for a thin posting."
                },
                "genuine_gaps": {
                    "type": "string",
                    "description": "Real, specific gaps between the candidate's experience and this role's requirements. Be honest — don't invent gaps to seem balanced, and don't paper over real ones. Keep to 2-3 sentences."
                },
                "transferable_strengths": {
                    "type": "string",
                    "description": "Which of the candidate's competencies genuinely transfer to this role, and why — cite specifics from their profile, not generic claims. Keep to 2-3 sentences."
                },
                "risk_factors": {
                    "type": "string",
                    "description": "Non-skill risks: seniority mismatch, domain mismatch, likely comp mismatch, stack dealbreakers the deterministic filter might have missed, company-stage risk given the candidate's stated preferences. Keep to 2-3 sentences."
                }
            },
            "required": ["match_score", "recommendation", "company_stage", "stage_confidence",
                         "genuine_gaps", "transferable_strengths", "risk_factors"]
        }
    })
}

pub struct Evaluator {
    api_key: String,
    model: String,
    client: reqwest::Client,
    profile_yaml: String,
}

impl Evaluator {
    pub fn new(profile: &serde_yaml::Value, model: String) -> Result<Self> {
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .ok()
            .filter(|k| !k.is_empty())
            .ok_or_else(|| anyhow!("ANTHROPIC_API_KEY is not set — put it in .env or export it"))?;
        Ok(Self {
            api_key,
            model,
            client: crate::sources::http::client().clone(),
            // Serialized once: the profile is identical for every job in a
            // run, and it's the largest part of the prompt.
            profile_yaml: serde_yaml::to_string(profile)
                .context("serializing profile.yaml for the prompt")?,
        })
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn build_user_prompt(&self, job: &JobRow, looks_truncated: bool) -> String {
        let description = crate::filters::strip_html(&job.description);
        let description = description.trim();
        let description = if description.is_empty() {
            "(no JD text available)"
        } else {
            description
        };

        // Without this note, a thin snippet reads to the model as "this
        // role lists no responsibilities", and it invents gaps and drops
        // the score for something the posting never claimed.
        let partial_note = if looks_truncated && !job.description.is_empty() {
            "\nNOTE: this description looks like a short snippet, not the full posting — it may \
             have been cut off mid-sentence. Do NOT lower match_score or invent genuine_gaps just \
             because a responsibility isn't mentioned here; judge fit on title, company, location, \
             and whatever specifics ARE present. If the snippet is too thin to say anything \
             meaningful about stack or seniority, say so in genuine_gaps rather than guessing.\n"
        } else {
            ""
        };

        format!(
            "CANDIDATE PROFILE:\n{profile}\n\n---\n\nJOB POSTING TO EVALUATE:\n\
             Company: {company}\nTitle: {title}\nLocation: {location}\nURL: {url}\n\
             Source: {source}\n{partial_note}\nDescription:\n{description}\n\n---\n\n\
             Call submit_evaluation with your structured assessment.",
            profile = self.profile_yaml,
            company = job.company,
            title = job.title,
            location = job.location,
            url = job.url,
            source = job.source,
        )
    }

    /// Call the model and validate that the tool response carries every
    /// required field.
    ///
    /// A forced `tool_choice` can still produce an incomplete object if
    /// `max_tokens` is hit mid-generation, so a short response is retried
    /// once with more room rather than failing the whole run on one job.
    pub async fn evaluate(&self, job: &JobRow, looks_truncated: bool) -> Result<Evaluation> {
        let user_content = self.build_user_prompt(job, looks_truncated);
        let mut max_tokens = 1536u32;
        let max_retries = 1;

        let mut last_err = None;
        for attempt in 0..=max_retries {
            let body = json!({
                "model": self.model,
                "max_tokens": max_tokens,
                "system": SYSTEM_PROMPT,
                "tools": [evaluation_schema()],
                "tool_choice": {"type": "tool", "name": "submit_evaluation"},
                "messages": [{"role": "user", "content": user_content}],
            });

            let resp = self
                .client
                .post(API_URL)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", API_VERSION)
                .header("content-type", "application/json")
                .json(&body)
                .send()
                .await
                .context("calling the Anthropic Messages API")?;

            let status = resp.status();
            let text = resp.text().await.context("reading the API response")?;
            if !status.is_success() {
                // A 4xx is a request problem that a retry won't fix (bad
                // key, bad model name, malformed schema) — fail fast and
                // say what the API said, rather than burning the retry.
                if status.as_u16() < 500 && status.as_u16() != 429 {
                    bail!("Anthropic API returned {status}: {}", truncate(&text, 500));
                }
                last_err = Some(anyhow!(
                    "Anthropic API returned {status}: {}",
                    truncate(&text, 300)
                ));
                continue;
            }

            match parse_response(&text) {
                Ok(eval) => return Ok(eval),
                Err(e) => {
                    last_err = Some(e);
                    // Most likely truncation mid-tool-call — give the
                    // retry more room.
                    max_tokens += 512;
                    if attempt == max_retries {
                        break;
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("evaluation failed for {}", job.url)))
    }
}

#[derive(Deserialize)]
struct MessagesResponse {
    #[serde(default)]
    content: Vec<ContentBlock>,
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    input: Option<Value>,
}

fn parse_response(text: &str) -> Result<Evaluation> {
    let resp: MessagesResponse =
        serde_json::from_str(text).context("parsing the Anthropic API response")?;

    let input = resp
        .content
        .iter()
        .find(|b| b.kind == "tool_use" && b.name.as_deref() == Some("submit_evaluation"))
        .and_then(|b| b.input.as_ref())
        .ok_or_else(|| {
            anyhow!(
                "model didn't call submit_evaluation (stop_reason={})",
                resp.stop_reason.as_deref().unwrap_or("unknown")
            )
        })?;

    let get_str = |key: &str| -> Option<String> {
        input
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|s| !s.is_empty())
    };
    let get_i64 = |key: &str| -> Option<i64> { input.get(key).and_then(Value::as_i64) };

    // The two decision-critical fields are required; the prose fields are
    // not. A response that scored the job but got cut off before writing
    // three paragraphs of rationale is still worth keeping — re-running it
    // would cost money to recover text that isn't what the score is for.
    let match_score = get_i64("match_score").ok_or_else(|| {
        anyhow!(
            "response is missing match_score: {}",
            truncate(&input.to_string(), 300)
        )
    })?;
    let recommendation = get_str("recommendation").ok_or_else(|| {
        anyhow!(
            "response is missing recommendation: {}",
            truncate(&input.to_string(), 300)
        )
    })?;

    Ok(Evaluation {
        match_score: match_score.clamp(0, 100),
        recommendation,
        genuine_gaps: get_str("genuine_gaps").unwrap_or_default(),
        transferable_strengths: get_str("transferable_strengths").unwrap_or_default(),
        risk_factors: get_str("risk_factors").unwrap_or_default(),
        // "unknown" is a real answer from the schema, not a parse failure
        // — Stage::parse returns None for it, which is exactly right.
        company_stage: get_str("company_stage").as_deref().and_then(Stage::parse),
        company_headcount: get_i64("company_headcount").filter(|h| *h > 0),
        stage_confidence: get_i64("stage_confidence"),
    })
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_complete_tool_response() {
        let body = r#"{
          "content": [{"type":"tool_use","name":"submit_evaluation","input":{
            "match_score": 87, "recommendation": "apply",
            "company_stage": "series-b", "company_headcount": 80, "stage_confidence": 70,
            "genuine_gaps": "g", "transferable_strengths": "s", "risk_factors": "r"
          }}],
          "stop_reason": "tool_use"
        }"#;
        let eval = parse_response(body).unwrap();
        assert_eq!(eval.match_score, 87);
        assert_eq!(eval.recommendation, "apply");
        assert_eq!(eval.company_stage, Some(Stage::SeriesB));
        assert_eq!(eval.company_headcount, Some(80));
        assert_eq!(eval.stage_confidence, Some(70));
    }

    #[test]
    fn unknown_stage_is_no_stage_not_an_error() {
        let body = r#"{"content":[{"type":"tool_use","name":"submit_evaluation","input":{
            "match_score": 50, "recommendation": "consider", "company_stage": "unknown",
            "stage_confidence": 10, "genuine_gaps":"g","transferable_strengths":"s","risk_factors":"r"
        }}]}"#;
        let eval = parse_response(body).unwrap();
        assert_eq!(eval.company_stage, None);
        assert_eq!(eval.match_score, 50);
    }

    #[test]
    fn a_truncated_response_keeps_the_score_if_the_critical_fields_survived() {
        // Exactly the failure the field ordering is designed around: the
        // long prose fields were cut off, the decision fields weren't.
        let body = r#"{"content":[{"type":"tool_use","name":"submit_evaluation","input":{
            "match_score": 72, "recommendation": "consider"
        }}],"stop_reason":"max_tokens"}"#;
        let eval = parse_response(body).unwrap();
        assert_eq!(eval.match_score, 72);
        assert_eq!(eval.genuine_gaps, "");
    }

    #[test]
    fn a_response_missing_the_decision_fields_is_an_error() {
        let body = r#"{"content":[{"type":"tool_use","name":"submit_evaluation","input":{
            "genuine_gaps": "g"
        }}],"stop_reason":"max_tokens"}"#;
        assert!(parse_response(body).is_err());
    }

    #[test]
    fn a_text_only_response_is_an_error_naming_the_stop_reason() {
        let body = r#"{"content":[{"type":"text","text":"I cannot"}],"stop_reason":"end_turn"}"#;
        let err = parse_response(body).unwrap_err().to_string();
        assert!(
            err.contains("end_turn"),
            "error should name the stop reason: {err}"
        );
    }

    #[test]
    fn out_of_range_scores_are_clamped() {
        let body = r#"{"content":[{"type":"tool_use","name":"submit_evaluation","input":{
            "match_score": 150, "recommendation": "apply"
        }}]}"#;
        assert_eq!(parse_response(body).unwrap().match_score, 100);
    }
}
