use super::{document, validate_message};
use serde_json::json;

#[test]
fn handshake_messages_are_schema_valid() {
    let doc = document();
    let fixtures = [
        (
            "hello",
            json!({"protocol":[2],"agentVersion":"0.1.0","bootId":"01J","capabilities":[],"observed":{},"pendingOpResults":[]}),
        ),
        ("challenge", json!({"nonce":"0123456789abcdef"})),
        ("auth", json!({"nodeId":"n1","signature":"signature"})),
        (
            "welcome",
            json!({"sessionId":"s1","heartbeatSec":15,"resyncSec":300,"serverTime":"2026-01-01T00:00:00Z"}),
        ),
        ("goaway", json!({"retryAfterMs":0})),
        ("ping", json!({})),
        ("pong", json!({})),
    ];

    for (kind, body) in fixtures {
        validate_message(&doc, kind, body);
    }
}
