# Tests

`test_child.rs` holds the `TestChild` contract tests, which re-execute the test
binary as their child processes. Its `planted_*` tests are inert unless
`ci/check_nextest_termination.py` plants them to prove nextest's per-test
termination (#1971).
