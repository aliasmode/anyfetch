use super::*;

/// A property without a top-level `type` (a bare `$ref` or `anyOf`) shows as "unknown" in
/// agent harnesses, and a `type` array is refused by some clients. So: one type string.
#[test]
fn every_tool_property_has_one_type() {
    for tool in Server::new().tool_router.list_all() {
        let properties = tool.input_schema["properties"].as_object().unwrap();
        for (name, schema) in properties {
            let at = format!("{}.{name}: {schema}", tool.name);
            assert!(schema["type"].is_string(), "{at}");
            for key in ["anyOf", "oneOf", "allOf", "$ref"] {
                assert!(schema.get(key).is_none(), "{at}");
            }
            if let Some(values) = schema["enum"].as_array() {
                assert!(values.iter().all(Value::is_string), "{at}");
            }
        }
    }
}
