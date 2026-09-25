//! One shared HTTP client for every source, plus the page-to-text
//! extraction the aggregator full-JD fetch needs.

use anyhow::{Context, Result};
use std::sync::OnceLock;
use std::time::Duration;

pub const USER_AGENT: &str = concat!(
    "job-search-pipeline/",
    env!("CARGO_PKG_VERSION"),
    " (personal use)"
);

pub const TIMEOUT: Duration = Duration::from_secs(20);
/// Shorter than `TIMEOUT` on purpose: the full-JD fetch is a best-effort
/// extra request per job, and one slow host must not stall a whole run.
pub const FULL_JD_TIMEOUT: Duration = Duration::from_secs(10);

static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

/// The process-wide client. Sharing one matters for more than tidiness:
/// it's what gives connection pooling across the ~130 company fetches, and
/// keeps a single place to change timeouts and redirect policy.
pub fn client() -> &'static reqwest::Client {
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(TIMEOUT)
            .connect_timeout(Duration::from_secs(8))
            .pool_max_idle_per_host(8)
            .build()
            .expect("building the shared HTTP client")
    })
}

/// GET and parse JSON, with the URL in the error on any failure — without
/// it, a run over 130 boards reports "error decoding response body" with
/// no way to tell which board.
pub async fn get_json<T: serde::de::DeserializeOwned>(url: &str) -> Result<T> {
    get_json_with(url, &[]).await
}

pub async fn get_json_with<T: serde::de::DeserializeOwned>(
    url: &str,
    query: &[(String, String)],
) -> Result<T> {
    let resp = client()
        .get(url)
        .query(query)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url}"))?;
    let body = resp
        .bytes()
        .await
        .with_context(|| format!("reading body of {url}"))?;
    serde_json::from_slice(&body).with_context(|| format!("parsing JSON from {url}"))
}

/// Fetch a page and return `(final_url, body)`. The final URL is the point
/// of this over `get_json`: the aggregator path needs to know where a
/// redirect actually landed to recognize an ATS.
pub async fn get_text_following_redirects(
    url: &str,
    timeout: Duration,
) -> Result<(String, String)> {
    let resp = client()
        .get(url)
        .timeout(timeout)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url}"))?;
    let final_url = resp.url().to_string();
    let body = resp
        .text()
        .await
        .with_context(|| format!("reading body of {url}"))?;
    Ok((final_url, body))
}

/// Rough main-content extraction for a page with no structured API: drop
/// script/style blocks (JS and CSS text is not content), strip the
/// remaining tags, collapse the whitespace. Will pick up some nav/footer
/// boilerplate — still far more use to the AI step than a one-sentence
/// aggregator snippet.
pub fn html_page_to_text(html: &str) -> String {
    let without_scripts = strip_elements(html, &["script", "style", "noscript", "svg"]);
    crate::filters::collapse_whitespace(&crate::filters::strip_html(&without_scripts))
}

/// Remove whole `<tag>...</tag>` regions, contents included.
fn strip_elements(html: &str, tags: &[&str]) -> String {
    let mut out = html.to_string();
    for tag in tags {
        let open = format!("<{tag}");
        let close = format!("</{tag}>");
        loop {
            let lower = out.to_lowercase();
            let Some(start) = lower.find(&open) else {
                break;
            };
            let end = match lower[start..].find(&close) {
                Some(rel) => start + rel + close.len(),
                // Unclosed tag — drop the rest of the document rather than
                // loop forever on malformed markup.
                None => out.len(),
            };
            out.replace_range(start..end, " ");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_script_and_style_contents() {
        let html = "<p>Real</p><script>var x = '<p>fake</p>';</script><style>p{color:red}</style>";
        let text = html_page_to_text(html);
        assert!(text.contains("Real"));
        assert!(!text.contains("fake"));
        assert!(!text.contains("color"));
    }

    #[test]
    fn unclosed_script_does_not_hang() {
        let text = html_page_to_text("<p>Real</p><script>never closed");
        assert!(text.contains("Real"));
    }
}
