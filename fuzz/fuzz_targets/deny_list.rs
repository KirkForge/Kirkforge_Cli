#![no_main]

use kf_code::session::access::deny_list::DenyList;
use libfuzzer_sys::fuzz_target;
use arbitrary::{Arbitrary, Unstructured};

#[derive(Arbitrary)]
struct FuzzInput {
    path_patterns: Vec<String>,
    url_patterns: Vec<String>,
    test_path: String,
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);
    if let Ok(input) = FuzzInput::arbitrary(&mut u) {
        let dl = DenyList::new(input.path_patterns, input.url_patterns);
        let _ = dl.is_path_denied(std::path::Path::new(&input.test_path));
        let _ = dl.is_url_denied(&input.test_path);
    }
});
