#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let input = std::str::from_utf8(data).unwrap_or("");
    for line in input.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let _ = serde_json::from_str::<serde_json::Value>(trimmed);
    }
});
