use super::{document, validate_message};
use serde_json::json;

#[test]
fn log_messages_are_schema_valid() {
    let doc = document();
    let fixtures = [
        ("logs.query", json!({"requestId":"r1"})),
        ("logs.result", json!({"requestId":"r1","records":[]})),
        ("logs.capabilities", json!({})),
    ];

    for (kind, body) in fixtures {
        validate_message(&doc, kind, body);
    }
}
