#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    for (start, ch) in text.char_indices() {
        if ch != '{' {
            continue;
        }
        let mut de =
            serde_json::Deserializer::from_str(&text[start..]).into_iter::<serde_json::Value>();
        if let Some(Ok(v)) = de.next() {
            if v.is_object() && v.get("type").is_some() {
                let _ = de.byte_offset();
            }
        }
    }
});
