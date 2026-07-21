use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    error::{DriftError, Result},
    model::{Trace, TraceRecord},
};

pub fn sha256_bytes(bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(bytes);
    let mut encoded = String::with_capacity(71);
    encoded.push_str("sha256:");
    for byte in digest.finalize() {
        use std::fmt::Write;
        write!(encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

pub fn canonical_digest<T: Serialize>(value: &T) -> Result<String> {
    let bytes = serde_json::to_vec(value).map_err(|error| {
        DriftError::new("E_INTERNAL", format!("serialize digest input: {error}"))
    })?;
    Ok(sha256_bytes(&bytes))
}

#[derive(Serialize)]
struct NormalizedTrace<'a> {
    session: &'a TraceRecord,
    events: &'a [TraceRecord],
}

pub fn normalized_trace_digest(trace: &Trace) -> Result<String> {
    canonical_digest(&NormalizedTrace {
        session: &trace.header,
        events: &trace.events,
    })
}
