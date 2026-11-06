#![allow(dead_code)]

const ON: &str = "on";

fn spelling_chain(value: &str) -> bool {
    value == "1" || value.eq_ignore_ascii_case("true")
}

fn ufcs_chain(value: &str) -> bool {
    str::eq_ignore_ascii_case(value, "yes") || PartialEq::eq(value, ON)
}

fn matched_set(value: &str) -> bool {
    matches!(value, "1" | "true" | "yes" | "on")
}

fn array_set(value: &str) -> bool {
    ["0", "false"].contains(&value)
}

fn denylist(value: &str) -> bool {
    !value.is_empty() && value != "0"
}

fn env_equals_one() -> bool {
    std::env::var("ANY_FLAG").is_ok_and(|value| value == "1")
}

fn main() {}
