// Contract tests for `contracts/openapi/statix-agent.yaml`.
//
// Keep the implementation inventory explicit. Inferring coverage from the
// Rust enums would make an omitted handler invisible to the test itself.

use std::collections::BTreeSet;

use jsonschema::JSONSchema;
use serde_json::{json, Value};

use crate::transport::protocol::encode;

const OPENAPI: &str = include_str!("../../../../contracts/openapi/statix-agent.yaml");

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
        implemented: true,
        tested: true,
    },
    Implementation {
        kind: "op_cancel",
        direction: Direction::ServerToAgent,
        implemented: true,
        tested: true,
    },
    Implementation {
        kind: "op_result",
        direction: Direction::AgentToServer,
        implemented: true,
        tested: true,
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
    Implementation { kind: "console.open", direction: Direction::ServerToAgent, implemented: true, tested: true },
    Implementation { kind: "console.opened", direction: Direction::AgentToServer, implemented: true, tested: true },
    Implementation { kind: "console.data", direction: Direction::AgentToServer, implemented: true, tested: true },
    Implementation { kind: "console.resize", direction: Direction::ServerToAgent, implemented: true, tested: true },
    Implementation { kind: "console.close", direction: Direction::ServerToAgent, implemented: true, tested: true },
    Implementation { kind: "console.error", direction: Direction::AgentToServer, implemented: true, tested: true },
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
fn unsupported_messages_are_reported_as_warnings() {
    for item in IMPLEMENTATIONS.iter().filter(|item| !item.implemented) {
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

mod route {
    include!("route.rs");
}

mod handshake {
    include!("handshake.rs");
}

mod state {
    include!("state.rs");
}

mod logs {
    include!("logs.rs");
}
