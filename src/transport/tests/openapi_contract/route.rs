use super::document;

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
