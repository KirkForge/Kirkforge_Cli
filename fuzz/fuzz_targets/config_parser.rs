#![no_main]

use kf_code::shared::Config;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let input = std::str::from_utf8(data).unwrap_or("");
    let _ = toml::from_str::<Config>(input);
});
