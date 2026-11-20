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
        ("fast", 9, ""),
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
