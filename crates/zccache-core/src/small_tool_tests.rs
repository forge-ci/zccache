use super::*;

#[test]
fn excerpt_is_bounded_and_keeps_the_end_of_stderr() {
    let mut stderr = "noise line\n".repeat(20_000).into_bytes();
    stderr.extend_from_slice("final: the real é failure".as_bytes());

    let diagnostic = SmallToolDiagnostic::completed("cc", Some(1), Duration::ZERO, &stderr);

    assert!(diagnostic.stderr_excerpt.len() <= STDERR_EXCERPT_LIMIT_BYTES + 3);
    assert!(diagnostic.stderr_excerpt.ends_with("the real é failure"));
    assert_eq!(diagnostic.stderr_bytes, stderr.len());
}

#[test]
fn truncation_never_exposes_a_value_without_its_name() {
    let line = format!("API_TOKEN={}\n", "s3cret".repeat(20_000));

    let diagnostic = SmallToolDiagnostic::completed("cc", Some(1), Duration::ZERO, line.as_bytes());

    assert!(!diagnostic.stderr_excerpt.contains("s3cret"));
}

#[test]
fn invalid_utf8_stderr_is_still_retained() {
    let diagnostic =
        SmallToolDiagnostic::completed("cc", Some(1), Duration::ZERO, b"bad \xff byte");

    assert!(diagnostic.stderr_excerpt.contains("byte"));
}

#[test]
fn secrets_are_redacted_in_each_common_shape() {
    for (input, secret) in [
        ("failed API_TOKEN=abc123", "abc123"),
        ("--password abc123 next", "abc123"),
        ("Authorization: Bearer abc123", "abc123"),
        ("password:abc123", "abc123"),
        ("fetch https://user:abc123@host/repo failed", "abc123"),
    ] {
        let redacted = redact_text(input);
        assert!(!redacted.contains(secret), "{input} -> {redacted}");
        assert!(redacted.contains(REDACTED), "{input} -> {redacted}");
    }
}

#[test]
fn ordinary_diagnostics_are_unchanged() {
    let text = "error: linking failed at https://host/repo\nsee log.txt:12";
    assert_eq!(redact_text(text), text);
}

#[test]
fn silent_success_is_not_noteworthy_but_anything_else_is() {
    let at =
        |code, stderr: &[u8]| SmallToolDiagnostic::completed("t", code, Duration::ZERO, stderr);
    assert!(!at(Some(0), b"").is_noteworthy());
    assert!(!at(Some(0), b"  \n").is_noteworthy());
    assert!(at(Some(0), b"note").is_noteworthy());
    assert!(at(Some(2), b"").is_noteworthy());
    assert!(at(None, b"").is_noteworthy());
    assert!(SmallToolDiagnostic::failed_to_run("t", Duration::ZERO, "gone").is_noteworthy());
}

#[test]
fn summary_identifies_tool_status_and_excerpt() {
    let summary =
        SmallToolDiagnostic::completed("taskkill", Some(128), Duration::ZERO, b"no such process")
            .summary();
    assert_eq!(summary, "taskkill: exited with code 128: no such process");
}
