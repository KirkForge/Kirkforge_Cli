#![no_main]

use libfuzzer_sys::fuzz_target;
use kf_code::shared::minify::{minify_with_map, revalidate};

fuzz_target!(|data: &str| {
    if let Some(minified) = minify_with_map(data, "rs", false) {
        let _ = revalidate("rs", &minified.text);
    }
});
