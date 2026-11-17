// Contract tests for `contracts/openapi/statix-agent.yaml`.
//
// Keep the implementation inventory explicit. Inferring coverage from the
// Rust enums would make an omitted handler invisible to the test itself.

use std::collections::BTreeSet;

use jsonschema::JSONSchema;
use serde_json::{Value, json};

use crate::transport::protocol::encode;

const OPENAPI: &str = include_str!("../../../contracts/openapi/statix-agent.yaml");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    AgentToServer,
    ServerToAgent,
}

#[derive(Debug, Clone, Copy)]
struct Implementation {
    kind: &'static str,
    direction: Direction,
    implemented: bool,
    tested: bool,
}

fn direction_name(direction: Direction) -> &'static str {
    match direction {
        Direction::AgentToServer => "agent -> server",
        Direction::ServerToAgent => "server -> agent",
    }
}

// This is intentionally exhaustive for code coverage, but it is not a copy
// of the OpenAPI list: the audit below compares this inventory to the spec.
const IMPLEMENTATIONS: &[Implementation] = &[
    Implementation {
        kind: "hello",
        direction: Direction::AgentToServer,
        implemented: true,
        tested: true,
    },
    Implementation {
        kind: "challenge",
        direction: Direction::ServerToAgent,
        implemented: true,
        tested: true,
    },
    Implementation {
        kind: "auth",
        direction: Direction::AgentToServer,
        implemented: true,
        tested: true,
    },
    Implementation {
        kind: "welcome",
        direction: Direction::ServerToAgent,
        implemented: true,
        tested: true,
    },
    Implementation {
        kind: "goaway",
        direction: Direction::ServerToAgent,
        implemented: true,
        tested: true,
    },
    Implementation {
        kind: "ping",
        direction: Direction::ServerToAgent,
        implemented: true,
        tested: true,
    },
    Implementation {
        kind: "pong",
        direction: Direction::AgentToServer,
        implemented: true,
        tested: true,
    },
    Implementation {
        kind: "desired",
        direction: Direction::ServerToAgent,
        implemented: true,
        tested: true,
    },
    Implementation {
        kind: "status",
        direction: Direction::AgentToServer,
        implemented: true,
        tested: true,
    },
    Implementation {
        kind: "event",
        direction: Direction::AgentToServer,
        implemented: true,
        tested: true,
    },
    Implementation {
        kind: "metrics",
        direction: Direction::AgentToServer,
        implemented: true,
        tested: true,
    },
    Implementation {
        kind: "system_info",
        direction: Direction::AgentToServer,
        implemented: true,
        tested: true,
    },
    Implementation {
        kind: "op_dispatch",
        direction: Direction::ServerToAgent,
        implemented: false,
        tested: false,
    },
    Implementation {
        kind: "op_cancel",
        direction: Direction::ServerToAgent,
        implemented: false,
        tested: false,
    },
    Implementation {
        kind: "op_result",
        direction: Direction::AgentToServer,
        implemented: false,
        tested: false,
    },
    Implementation {
        kind: "logs.subscribe",
        direction: Direction::ServerToAgent,
        implemented: false,
        tested: false,
    },
    Implementation {
        kind: "logs.unsubscribe",
        direction: Direction::ServerToAgent,
        implemented: false,
        tested: false,
    },
    Implementation {
        kind: "logs.chunk",
        direction: Direction::AgentToServer,
        implemented: false,
        tested: false,
    },
    Implementation {
        kind: "logs.query",
        direction: Direction::ServerToAgent,
        implemented: true,
        tested: true,
    },
    Implementation {
        kind: "logs.result",
        direction: Direction::AgentToServer,
        implemented: true,
        tested: true,
    },
    Implementation {
        kind: "logs.capabilities",
        direction: Direction::AgentToServer,
        implemented: true,
        tested: true,
    },
];

fn document() -> Value {
    let yaml: serde_yaml::Value = serde_yaml::from_str(OPENAPI).expect("OpenAPI YAML is valid");
    serde_json::to_value(yaml).expect("OpenAPI YAML converts to JSON")
}

fn spec_message_names(doc: &Value) -> BTreeSet<String> {
    doc["components"]["x-messages"]
        .as_object()
        .expect("OpenAPI components.x-messages exists")
        .values()
        .map(|message| message["name"].as_str().expect("message name").to_owned())
        .collect()
}

#[test]
fn agent_websocket_route_is_declared_as_a_websocket_upgrade() {
    let doc = document();
    let operation = &doc["paths"]["/ws/agent"]["get"];
    assert_eq!(operation["operationId"], "agentWebSocket");
    assert_eq!(
        operation["responses"]["101"]["description"],
        "Switching Protocols; subsequent traffic is JSON Envelope frames."
    );
    assert_eq!(operation["x-websocket"]["protocol"], "statix.node/v2");
    assert_eq!(operation["x-websocket"]["direction"], "bidirectional");
}

fn schema_name_for_message(doc: &Value, kind: &str) -> String {
    doc["components"]["x-messages"]
        .as_object()
        .expect("OpenAPI components.x-messages exists")
        .values()
        .find(|message| message["name"] == kind)
        .unwrap_or_else(|| panic!("message {kind} is present in x-messages"))["payload"]["$ref"]
        .as_str()
        .and_then(|reference| reference.rsplit('/').next())
        .unwrap_or_else(|| panic!("message {kind} has a payload ref"))
        .to_owned()
}

fn validate_message(doc: &Value, kind: &'static str, body: Value) {
    let schema_name = schema_name_for_message(doc, kind);
    let schemas = doc["components"]["schemas"].clone();
    let wrapper = json!({
        "$ref": format!("#/components/schemas/{schema_name}"),
        "components": { "schemas": schemas },
    });
    let validator = JSONSchema::compile(&wrapper)
        .unwrap_or_else(|error| panic!("could not compile schema for {kind}: {error}"));
    let frame: Value = serde_json::from_str(&encode(kind, body))
        .unwrap_or_else(|error| panic!("{kind} could not be encoded as JSON: {error}"));
    if let Err(errors) = validator.validate(&frame) {
        let messages = errors
            .map(|error| error.to_string())
            .collect::<Vec<_>>()
            .join("; ");
        panic!("{kind} frame violates OpenAPI schema: {messages}");
    }
}

#[test]
fn every_spec_message_has_an_explicit_implementation_entry() {
    let doc = document();
    let spec = spec_message_names(&doc);
    let inventory = IMPLEMENTATIONS
        .iter()
        .map(|item| item.kind)
        .collect::<BTreeSet<_>>();
    let mut errors = Vec::new();

    for kind in spec
        .iter()
        .filter(|kind| !inventory.contains(kind.as_str()))
    {
        errors.push(format!(
            "ERROR contract: spec message `{kind}` has no implementation entry"
        ));
    }
    for kind in inventory.iter().filter(|kind| !spec.contains(**kind)) {
        errors.push(format!(
            "ERROR contract: code message `{kind}` is absent from OpenAPI"
        ));
    }

    assert!(errors.is_empty(), "{}\n", errors.join("\n"));
}

#[test]
fn every_implemented_message_is_schema_valid() {
    let doc = document();
    let mut failures = Vec::new();
    for item in IMPLEMENTATIONS.iter().filter(|item| item.implemented) {
        let body = match item.kind {
            "hello" => {
                json!({"protocol":[2],"agentVersion":"0.1.0","bootId":"01J","capabilities":[],"observed":{},"pendingOpResults":[]})
            }
            "challenge" => json!({"nonce":"0123456789abcdef"}),
            "auth" => json!({"nodeId":"n1","signature":"signature"}),
            "welcome" => {
                json!({"sessionId":"s1","heartbeatSec":15,"resyncSec":300,"serverTime":"2026-01-01T00:00:00Z"})
            }
            "goaway" => json!({"retryAfterMs":0}),
            "ping" | "pong" => json!({}),
            "desired" => json!({"nodeId":"n1","generation":0,"runtimes":[],"workloads":[]}),
            "status" => {
                json!({"id":"r1","kind":"Runtime","nodeId":"n1","observedGeneration":0,"phase":"Pending","conditions":[],"reportedAt":"2026-01-01T00:00:00Z"})
            }
            "event" => {
                json!({"id":"e1","nodeId":"n1","severity":"info","reason":"Test","message":"test","at":"2026-01-01T00:00:00Z"})
            }
            "metrics" => json!({"ts":1}),
            "system_info" => json!({}),
            "logs.query" => json!({"requestId":"r1"}),
            "logs.result" => json!({"requestId":"r1","records":[]}),
            "logs.capabilities" => json!({}),
            other => panic!("implemented message `{other}` needs a contract fixture"),
        };
        let result = std::panic::catch_unwind(|| validate_message(&doc, item.kind, body));
        if result.is_err() {
            failures.push(item.kind);
        }
    }
    assert!(
        failures.is_empty(),
        "schema failures: {}",
        failures.join(", ")
    );
}

#[test]
fn unsupported_messages_are_reported_as_warnings() {
    let unsupported = IMPLEMENTATIONS
        .iter()
        .filter(|item| !item.implemented)
        .collect::<Vec<_>>();
    for item in &unsupported {
        eprintln!(
            "WARNING contract: `{}` ({}) is specified but not implemented or executable-tested",
            item.kind,
            direction_name(item.direction)
        );
    }
}

#[test]
fn implemented_messages_have_behavior_tests_registered() {
    let missing = IMPLEMENTATIONS
        .iter()
        .filter(|item| item.implemented && !item.tested)
        .map(|item| item.kind)
        .collect::<Vec<_>>();
    assert!(
        missing.is_empty(),
        "implemented messages without behavior tests: {}",
        missing.join(", ")
    );
}
