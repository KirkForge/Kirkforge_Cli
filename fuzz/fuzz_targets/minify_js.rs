#![no_main]

use kf_code::shared::minify::minify_with_map;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &str| {
    let _ = minify_with_map(data, "js", false);
});
