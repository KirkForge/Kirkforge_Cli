#![no_main]

use libfuzzer_sys::fuzz_target;
use kf_code::shared::minify::minify_with_map;

fuzz_target!(|data: &str| {
    let _ = minify_with_map(data, "rs", false);
});
