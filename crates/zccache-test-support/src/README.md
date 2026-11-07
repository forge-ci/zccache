# Source

`lib.rs` owns the cross-platform filesystem fixture builders and cleanup guards.

`test_child.rs` owns test subprocesses: mandatory deadlines, continuous pipe
draining with bounded retention, process-group cleanup and bounded reaping (#1971).
