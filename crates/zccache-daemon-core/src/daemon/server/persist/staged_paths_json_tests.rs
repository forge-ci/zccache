//! #1983: artifact JSON paths must be rewritten after decoding escapes.
use super::*;

#[test]
fn escaped_artifact_paths_round_trip_as_valid_json() {
    let private = Path::new(r"C:\private\staging");
    let requested = NormalizedPath::new("/requested/quote\"back\\slash/libfoo.rmeta");
    let original = serde_json::json!({
        "artifact": r"C:\private\staging\libfoo.rmeta",
        "emit": "metadata",
    });
    let original = format!("{original}\n");
    let canonical = canonicalize_staged_output_bytes(original.as_bytes(), private);
    let value: serde_json::Value = serde_json::from_slice(&canonical).unwrap();
    assert_eq!(
        value["artifact"],
        format!("{STAGED_OUTPUT_REMAP_ROOT}/libfoo.rmeta")
    );
    let rehydrated = rehydrate_staged_output_bytes(&canonical, std::slice::from_ref(&requested));
    let value: serde_json::Value = serde_json::from_slice(&rehydrated).unwrap();
    assert_eq!(value["artifact"], requested.to_string_lossy().as_ref());
    assert_eq!(value["emit"], "metadata");
    assert_eq!(rehydrated.last(), Some(&b'\n'));
}
