#![no_main]

use kf_code::adapters::ollama_ndjson::{parse_ndjson_lines, OllamaNdjsonConfig};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let input = std::str::from_utf8(data).unwrap_or("");
    let _ = parse_ndjson_lines(input, &OllamaNdjsonConfig::GLM);
});
