#![allow(dead_code)]

mod policy {
    pub fn colocate_requested() -> bool {
        false
    }
}

enum Mode {
    Off,
    On,
    Consistent,
}

// An enum mode owns its vocabulary, even when it accepts boolean spellings.
fn enum_mode(value: &str) -> Option<Mode> {
    match value {
        "1" | "true" => Some(Mode::On),
        "0" | "false" => Some(Mode::Off),
        "consistent" => Some(Mode::Consistent),
        _ => None,
    }
}

// A numeric limit with a sentinel is not truthiness.
fn limit(value: &str) -> Option<u32> {
    if value.eq_ignore_ascii_case("unlimited") || value == "0" {
        return None;
    }
    value.parse().ok()
}

// Typed accessors are how foreign and owned switches should be consumed.
fn typed_accessor() -> bool {
    policy::colocate_requested()
}

// One spelling with no environment read or emptiness check is ordinary data.
fn sentinel(value: &str) -> bool {
    value == "true"
}

// Environment reads compared with non-boolean strings are enum-like modes.
fn env_mode() -> bool {
    std::env::var("MODE").is_ok_and(|value| value == "fast")
}

fn main() {}
