use super::*;

fn jq(body: &str, program: &str) -> Result<String, ToolError> {
    evaluate(body, program, Instant::now() + TIMEOUT)
}

fn value(body: &str, program: &str) -> serde_json::Value {
    serde_json::from_str(&jq(body, program).unwrap()).unwrap()
}

#[test]
fn selects_paths_and_runs_filters() {
    let body = r#"{"prop":{"arr":[0,1,2,3,{"value":"five"}]},"items":[{"name":"a","x":1},{"name":"b","x":2}]}"#;
    assert_eq!(value(body, ".prop.arr[4].value"), "five");
    assert_eq!(value(body, ".items[].name"), serde_json::json!(["a", "b"]));
    assert_eq!(
        value(body, ".items | map(select(.x > 1))"),
        serde_json::json!([{"name": "b", "x": 2}])
    );
    assert_eq!(value(body, "keys"), serde_json::json!(["items", "prop"]));
    assert_eq!(value(body, ".prop.arr[-2]"), 3);
    assert_eq!(jq(body, "empty").unwrap_err().code, "no_match");
    assert_eq!(jq("<html>", ".").unwrap_err().code, "not_json");
}

#[test]
fn rejects_bad_programs_and_the_environment() {
    let error = jq("{}", ".a[").unwrap_err();
    assert_eq!(error.code, "invalid_request");
    assert!(
        error.message.contains("does not parse"),
        "{}",
        error.message
    );
    for program in [
        "env",
        "$ENV",
        "env.PATH",
        "input",
        "halt",
        "debug",
        "now | stderr",
    ] {
        let error = jq("{}", program).unwrap_err();
        assert_eq!(error.code, "invalid_request", "{program}");
        assert!(
            error.message.contains("not defined"),
            "{program}: {}",
            error.message
        );
    }
    assert_eq!(
        jq("1", "error(\"no\")").unwrap_err().code,
        "invalid_request"
    );
}

#[test]
fn stops_a_program_that_never_ends() {
    let error = evaluate("1", "repeat(1)", Instant::now()).unwrap_err();
    assert_eq!(error.code, "timeout");
}

#[test]
fn rejects_deep_input_but_not_brackets_in_strings() {
    let deep = format!("{}{}", "[".repeat(MAX_DEPTH + 1), "]".repeat(MAX_DEPTH + 1));
    let error = jq(&deep, ".").unwrap_err();
    assert!(
        error.message.contains("deeper than 256"),
        "{}",
        error.message
    );
    let edge = format!("{}{}", "[".repeat(MAX_DEPTH), "]".repeat(MAX_DEPTH));
    assert!(jq(&edge, ".").is_ok());
    let brackets = format!(r#"{{"a":"{}\"[{{"}}"#, "[".repeat(MAX_DEPTH + 1));
    assert_eq!(
        value(&brackets, ".a").as_str().unwrap().len(),
        MAX_DEPTH + 4
    );
}

#[test]
fn keeps_only_the_codes_the_child_can_send() {
    assert_eq!(known_code("no_match"), "no_match");
    assert_eq!(known_code("anything"), "invalid_request");
}
