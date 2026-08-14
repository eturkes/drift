use std::io::{BufReader, Cursor};

use drift_observer::{json::ensure_unique_keys, parse::parse_trace};
use proptest::{collection, prelude::*};
use serde_json::{Map, Number, Value};

fn json_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|value| Value::Number(Number::from(value))),
        "[ -~]{0,64}".prop_map(Value::String),
    ];

    leaf.prop_recursive(4, 64, 8, |inner| {
        prop_oneof![
            collection::vec(inner.clone(), 0..8).prop_map(Value::Array),
            collection::btree_map("[A-Za-z0-9_]{0,24}", inner, 0..8)
                .prop_map(|entries| Value::Object(Map::from_iter(entries))),
        ]
    })
}

fn parse_with_capacity(
    raw: &[u8],
    capacity: usize,
) -> drift_observer::Result<drift_observer::Trace> {
    parse_trace(BufReader::with_capacity(capacity, Cursor::new(raw)))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn serde_json_output_always_has_unique_object_keys(value in json_value()) {
        let bytes = serde_json::to_vec(&value).expect("generated JSON value serializes");
        prop_assert!(ensure_unique_keys(&bytes).is_ok());
    }

    #[test]
    fn duplicate_object_keys_are_always_rejected(
        key in "[A-Za-z0-9_]{1,24}",
        first in any::<i64>(),
        second in any::<i64>(),
    ) {
        let key = serde_json::to_string(&key).expect("generated key serializes");
        let bytes = format!("{{{key}:{first},{key}:{second}}}");
        prop_assert!(ensure_unique_keys(bytes.as_bytes()).is_err());
    }

    #[test]
    fn streaming_boundaries_do_not_change_trace_semantics(
        actor_suffix in "[A-Za-z0-9_-]{0,24}",
        message in "[ -~]{0,256}",
        trailing_newline in any::<bool>(),
        capacity in 1usize..128,
    ) {
        let actor = if actor_suffix.is_empty() {
            "agent".to_owned()
        } else {
            format!("agent:{actor_suffix}")
        };
        let session = serde_json::json!({
            "kind": "session",
            "schema": "drift.trace/v1",
            "id": "session",
            "task": "property check",
            "completeness": "partial",
        });
        let event = serde_json::json!({
            "kind": "message",
            "id": "event",
            "actor": actor,
            "text": message,
        });
        let mut raw = format!("{session}\n{event}").into_bytes();
        if trailing_newline {
            raw.push(b'\n');
        }

        let chunked = parse_with_capacity(&raw, capacity).expect("generated trace is valid");
        let contiguous = parse_with_capacity(&raw, raw.len().max(1)).expect("generated trace is valid");
        prop_assert_eq!(chunked.header, contiguous.header);
        prop_assert_eq!(chunked.events, contiguous.events);
        prop_assert_eq!(chunked.input_digest, contiguous.input_digest);
        prop_assert_eq!(chunked.warnings, contiguous.warnings);
    }
}
