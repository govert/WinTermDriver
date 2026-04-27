//! Validates documented protocol fixtures against the real IPC dispatcher.

use wtd_ipc::message::{parse_envelope, Envelope};

#[test]
fn documented_v1_current_envelopes_parse() {
    let raw = include_str!("../../../docs/protocol/examples/v1-current-envelopes.json");
    let envelopes: Vec<Envelope> = serde_json::from_str(raw).expect("fixture must be valid JSON");

    assert!(
        !envelopes.is_empty(),
        "protocol fixture should contain representative envelopes"
    );

    for envelope in envelopes {
        parse_envelope(&envelope).unwrap_or_else(|err| {
            panic!(
                "fixture envelope type {} with id {} failed to parse: {err}",
                envelope.msg_type, envelope.id
            )
        });
    }
}
