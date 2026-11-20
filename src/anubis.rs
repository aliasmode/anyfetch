use crate::ToolError;
use dom_query::Document;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use url::Url;

#[derive(Deserialize)]
pub(crate) struct Challenge {
    rules: Rules,
    challenge: Data,
    #[serde(skip)]
    base_prefix: String,
}

#[derive(Deserialize)]
struct Rules {
    algorithm: String,
    difficulty: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Data {
    id: String,
    random_data: String,
}

fn unsupported() -> ToolError {
    ToolError::new(
        "challenge_unsupported",
        "Malformed or unsupported Anubis challenge",
    )
}

pub(crate) fn parse(html: &str) -> Result<Option<Challenge>, ToolError> {
    let document = Document::from(html);
    let selection = document.select("script#anubis_challenge");
    let Some(script) = selection.iter().next() else {
        return Ok(None);
    };
    let mut challenge: Challenge =
        serde_json::from_str(&script.text()).map_err(|_| unsupported())?;
    let config = |selector| -> Result<String, ToolError> {
        match document.select(selector).iter().next() {
            Some(script) => serde_json::from_str(&script.text()).map_err(|_| unsupported()),
            None => Ok(String::new()),
        }
    };
    challenge.base_prefix = config("script#anubis_base_prefix")?;
    if challenge.rules.algorithm != "fast"
        || challenge.rules.difficulty > 8
        || challenge.challenge.id.is_empty()
        || challenge.challenge.random_data.is_empty()
        || !config("script#anubis_public_url")?.is_empty()
    {
        return Err(unsupported());
    }
    Ok(Some(challenge))
}

/// Bounds proof work so a hostile difficulty cannot pin a core.
const DEADLINE: Duration = Duration::from_secs(30);

/// Tells blocking work to stop when the async side that waits for it goes away.
pub(crate) struct CancelOnDrop(pub(crate) Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

fn solve(
    data: &str,
    difficulty: usize,
    cancelled: &AtomicBool,
) -> Result<(i64, String), ToolError> {
    let deadline = Instant::now() + DEADLINE;
    let mut prefix = Sha256::new();
    prefix.update(data.as_bytes());
    for nonce in 0..=i64::MAX {
        if cancelled.load(Ordering::Relaxed) {
            return Err(ToolError::new("cancelled", "Request cancelled"));
        }
        if nonce % 4096 == 0 && Instant::now() > deadline {
            return Err(ToolError::new(
                "challenge_failed",
                "Anubis proof took too long",
            ));
        }
        let mut hasher = prefix.clone();
        hasher.update(nonce.to_string().as_bytes());
        let hash = hasher.finalize();
        if hash[..difficulty / 2].iter().all(|byte| *byte == 0)
            && (difficulty.is_multiple_of(2) || hash[difficulty / 2] < 16)
        {
            return Ok((nonce, crate::hex(&hash)));
        }
    }
    Err(ToolError::new(
        "challenge_failed",
        "Anubis nonce range exhausted",
    ))
}

impl Challenge {
    pub(crate) async fn completion_url(self, page_url: &str) -> Result<Url, ToolError> {
        let mut url = Url::parse(page_url).map_err(|_| unsupported())?;
        url.set_path(&format!(
            "{}/.within.website/x/cmd/anubis/api/pass-challenge",
            self.base_prefix.trim_end_matches('/')
        ));
        url.set_query(None);
        url.set_fragment(None);
        let started = Instant::now();
        let guard = CancelOnDrop(Arc::new(AtomicBool::new(false)));
        let cancelled = guard.0.clone();
        let (nonce, response) = tokio::task::spawn_blocking(move || {
            solve(
                &self.challenge.random_data,
                self.rules.difficulty,
                &cancelled,
            )
        })
        .await
        .map_err(|_| ToolError::new("challenge_failed", "Anubis proof worker failed"))??;
        url.query_pairs_mut()
            .append_pair("id", &self.challenge.id)
            .append_pair("response", &response)
            .append_pair("nonce", &nonce.to_string())
            .append_pair("redir", page_url)
            .append_pair(
                "elapsedTime",
                &(started.elapsed().as_secs_f64() * 1000.0).to_string(),
            );
        Ok(url)
    }
}

#[cfg(test)]
#[path = "anubis.test.rs"]
mod tests;
