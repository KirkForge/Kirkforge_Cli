#![no_main]

use kf_code::shared::minify::minify_content_by_ext;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &str| {
    let _ = minify_content_by_ext(data, "css", false);
    let _ = minify_content_by_ext(data, "scss", false);
});
