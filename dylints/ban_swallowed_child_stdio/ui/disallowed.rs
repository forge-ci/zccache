// Every output-stream null sink, however it is spelled.
use std::process::{Command, Stdio};
use Stdio as Alias;

#[allow(dead_code)]
enum StreamMode {
    Null,
    Piped,
}

type ModeAlias = StreamMode;

#[allow(dead_code)]
struct Streams {
    stderr: StreamMode,
}

fn sink(_: StreamMode) {}

fn main() {
    let mut command = Command::new("tool");
    command.stdout(Stdio::null());
    command.stderr(std::process::Stdio::null());
    command.stderr(Alias::null());
    Command::stderr(&mut command, Stdio::null());
    let function_item = Stdio::null;
    command.stdout(function_item());
    let later = Stdio::null();
    command.stdin(later);
    sink(StreamMode::Null);
    sink(ModeAlias::Null);
    let _ = Streams {
        stderr: StreamMode::Null,
    };
}
