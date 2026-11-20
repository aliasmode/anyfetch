use super::*;

#[test]
fn adds_the_server_to_an_mcp_file_and_keeps_the_rest() {
    let current = r#"{"mcpServers": {"other": {"command": "x"}}, "theme": "dark"}"#;
    let config: Value =
        serde_json::from_str(&with_server(current, "/bin/anyfetch").unwrap()).unwrap();
    assert_eq!(config["theme"], "dark");
    assert_eq!(config["mcpServers"]["other"]["command"], "x");
    assert_eq!(
        config["mcpServers"]["anyfetch"],
        json!({ "command": "/bin/anyfetch", "args": ["mcp"] })
    );
    assert!(
        with_server("", "/bin/anyfetch")
            .unwrap()
            .contains("anyfetch")
    );
    // A file with comments or errors is left alone.
    assert!(with_server("{ // note\n}", "/bin/anyfetch").is_none());
    assert!(with_server("[]", "/bin/anyfetch").is_none());
}
