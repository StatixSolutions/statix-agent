use super::{document, validate_message};
use serde_json::json;

#[test]
fn state_and_telemetry_messages_are_schema_valid() {
    let doc = document();
    let fixtures = [
        (
            "desired",
            json!({"nodeId":"n1","generation":0,"runtimes":[],"workloads":[]}),
        ),
        (
            "status",
            json!({"id":"r1","kind":"Runtime","nodeId":"n1","observedGeneration":0,"phase":"Pending","conditions":[],"reportedAt":"2026-01-01T00:00:00Z"}),
        ),
        (
            "event",
            json!({"id":"e1","nodeId":"n1","severity":"info","reason":"Test","message":"test","at":"2026-01-01T00:00:00Z"}),
        ),
        ("metrics", json!({"ts":1})),
        ("system_info", json!({})),
    ];

    for (kind, body) in fixtures {
        validate_message(&doc, kind, body);
    }
}
