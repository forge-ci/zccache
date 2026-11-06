// Closing stdin, streaming, log-file capture and a reasoned detached child.
use std::process::{Command, Stdio};

#[allow(dead_code)]
enum StreamMode {
    Null,
    Piped,
}

struct Spec;

impl Spec {
    fn stdin(self, _: StreamMode) -> Self {
        self
    }
}

#[allow(dead_code)]
struct Streams {
    stdin: StreamMode,
}

fn main() {
    let mut command = Command::new("tool");
    command.stdin(Stdio::null());
    Command::stdin(&mut command, Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::inherit());
    if let Ok(log) = std::fs::File::create("tool.log") {
        command.stderr(Stdio::from(log));
    }
    let closed = std::env::args().count() > 1;
    let _ = Spec.stdin(if closed {
        StreamMode::Null
    } else {
        StreamMode::Piped
    });
    let _ = Streams {
        stdin: StreamMode::Null,
    };

    #[allow(ban_swallowed_child_stdio, reason = "detached: nothing can read it")]
    command.stdout(Stdio::null());
}
