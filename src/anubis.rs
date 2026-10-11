use crate::ToolError;
use dom_query::Document;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
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
        || challenge.rules.difficulty > 64
        || challenge.challenge.id.is_empty()
        || challenge.challenge.random_data.is_empty()
        || !config("script#anubis_public_url")?.is_empty()
    {
        return Err(unsupported());
    }
    Ok(Some(challenge))
}

struct CancelOnDrop(Arc<AtomicBool>);

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
    let mut prefix = Sha256::new();
    prefix.update(data.as_bytes());
    for nonce in 0..=i64::MAX {
        if cancelled.load(Ordering::Relaxed) {
            return Err(ToolError::new("cancelled", "Request cancelled"));
        }
        let mut hasher = prefix.clone();
        hasher.update(nonce.to_string().as_bytes());
        let hash = hasher.finalize();
        if hash[..difficulty / 2].iter().all(|byte| *byte == 0)
            && (difficulty.is_multiple_of(2) || hash[difficulty / 2] < 16)
        {
            return Ok((nonce, format!("{hash:x}")));
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
mod tests {
    use super::*;

    #[test]
    fn proof_matches_independent_utf8_and_nibble_vectors() {
        for (data, difficulty, nonce, digest) in [
            (
                "fixture-seed",
                1,
                0,
                "0fef85309f11a50dec2e738d961c6c7dcfd9ac10d183f8f33538a9f0b5305562",
            ),
            (
                "fixture-seed",
                2,
                291,
                "00e637e9abfbddeb3edefde7294f37bf078e20ee5f51163b99847a5efca0c0bc",
            ),
            (
                "café 日本語",
                3,
                3302,
                "0000fc40a11d18980948b8e0c291e0d64d07bd26b1372ac3c7391bb6d88a49ab",
            ),
        ] {
            assert_eq!(
                solve(data, difficulty, &AtomicBool::new(false)).unwrap(),
                (nonce, digest.into())
            );
        }
    }

    #[test]
    fn parses_only_supported_structural_challenges() {
        assert!(parse("<html>ordinary page</html>").unwrap().is_none());
        for (algorithm, difficulty, extra) in [
            ("fast", 65, ""),
            ("unknown", 1, ""),
            ("fast", 1, "<script id=anubis_base_prefix>{}</script>"),
            (
                "fast",
                1,
                "<script id=anubis_public_url>\"https://auth.example\"</script>",
            ),
        ] {
            let html = format!(
                r#"<script id=anubis_challenge>{{"rules":{{"algorithm":"{algorithm}","difficulty":{difficulty}}},"challenge":{{"id":"synthetic","randomData":"fixture-seed"}}}}</script>{extra}"#
            );
            assert_eq!(parse(&html).err().unwrap().code, "challenge_unsupported");
        }
    }

    #[tokio::test]
    async fn dropping_guard_stops_blocking_proof_work() {
        let guard = CancelOnDrop(Arc::new(AtomicBool::new(false)));
        let cancelled = guard.0.clone();
        let worker = tokio::task::spawn_blocking(move || solve("synthetic", 64, &cancelled));
        drop(guard);
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), worker)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.unwrap_err().code, "cancelled");
    }
}
