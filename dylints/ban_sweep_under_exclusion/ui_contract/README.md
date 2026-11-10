# Real materialization lifetime contract

These fixtures compile against daemon-core's feature-gated test bridge, whose
lease wrapper contains the real private MaterializationPayloads type. The failing
case must report an owner borrow violation; the positive control proves access
and imports work. They are outside Cargo examples because native Rust failures
must not break the dependency build used to prepare the compiler harness.
