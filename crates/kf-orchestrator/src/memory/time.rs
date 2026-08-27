//! Tiny time helpers used by the store + adapters. ponytail: avoids pulling
//! `chrono` for two callers — both just need an ISO-8601-ish UTC stamp.

pub fn iso_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (y, mo, d, h, mi, s) = unix_to_ymdhms(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

pub fn iso_now_minus_ms(ms: i64) -> String {
    let now_millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let secs = (now_millis - ms) / 1000;
    let (y, mo, d, h, mi, s) = unix_to_ymdhms(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

pub fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Pseudo-random u32 without a `rand` dep. PID × timestamp XOR with a
/// Knuth-multiplicative salt. Enough for id-uniqueness inside one process.
pub fn cheap_random_u32() -> u32 {
    let t = now_millis() as u32;
    let pid = std::process::id();
    t ^ pid.wrapping_mul(2654435761)
}

/// Howard Hinnant's civil-from-days algorithm.
pub fn unix_to_ymdhms(ts: i64) -> (i64, i64, i64, i64, i64, i64) {
    let days = ts.div_euclid(86400);
    let rem = ts.rem_euclid(86400);
    let h = rem / 3600;
    let mi = (rem % 3600) / 60;
    let s = rem % 60;
    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, h, mi, s)
}
