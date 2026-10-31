// A scan reports plain errors; the fail-open caller decides the outcome.
#[derive(Debug)]
struct AliasScanError(String);

enum CompileExecResult {
    Uncached,
}

fn scan() -> Result<(), AliasScanError> {
    Err(AliasScanError("compiler failed".to_owned()))
}

fn main() {
    let _ = scan();
    let _ = CompileExecResult::Uncached;
}
