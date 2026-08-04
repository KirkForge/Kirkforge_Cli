use anyhow::Context;
use std::io::{self, Read};
use std::path::PathBuf;

/// Default maximum input size: 50 MiB.
pub const DEFAULT_MAX_INPUT_SIZE: usize = 50 * 1024 * 1024;

const CHUNK_SIZE: usize = 4096;

/// Error returned when input data exceeds the configured maximum size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputTooLarge {
    limit: usize,
    got: usize,
}

impl InputTooLarge {
    /// Create an [`InputTooLarge`] with the given limit and observed size.
    #[must_use]
    pub const fn new(limit: usize, got: usize) -> Self {
        Self { limit, got }
    }

    /// Configured maximum size that was exceeded.
    #[must_use]
    pub const fn limit(self) -> usize {
        self.limit
    }

    /// Observed size of the input that exceeded the limit.
    #[must_use]
    pub const fn got(self) -> usize {
        self.got
    }
}

impl std::fmt::Display for InputTooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "input exceeds max size of {} bytes (got {} bytes)",
            self.limit(),
            self.got()
        )
    }
}

impl std::error::Error for InputTooLarge {}

/// Resolve the effective maximum input size from an optional CLI override.
///
/// Falls back to [`DEFAULT_MAX_INPUT_SIZE`] and clamps the result to at least
/// one byte so the limit is always usable.
#[must_use]
pub fn max_input_size(cli_max: Option<usize>) -> usize {
    cli_max.unwrap_or(DEFAULT_MAX_INPUT_SIZE).max(1)
}

/// Read input from a file, or from stdin if `file` is `None`.
///
/// Returns [`InputTooLarge`] if the source exceeds `max_size`. Files are
/// rejected early via metadata when possible; all sources are read in bounded
/// chunks to limit memory growth.
#[must_use = "the read input is required to run the pipeline"]
pub fn read_input(file: Option<PathBuf>, max_size: usize) -> anyhow::Result<String> {
    if let Some(p) = file {
        let mut f =
            std::fs::File::open(&p).with_context(|| format!("cannot open {}", p.display()))?;

        // Fast-path: reject obviously oversized regular files before allocating.
        if let Ok(meta) = f.metadata() {
            let len = meta.len();
            if len > max_size as u64 {
                return Err(
                    InputTooLarge::new(max_size, len.try_into().unwrap_or(usize::MAX)).into(),
                );
            }
        }

        let mut buf = Vec::with_capacity(initial_capacity(max_size));
        read_limited(&mut f, &mut buf, max_size)
            .with_context(|| format!("failed while reading {}", p.display()))?;
        return String::from_utf8(buf)
            .map_err(|e| anyhow::anyhow!("{} is not valid UTF-8: {e}", p.display()));
    }

    // Read incrementally so we can fail early on oversized stdin.
    let mut buf = Vec::with_capacity(initial_capacity(max_size));
    read_limited(&mut io::stdin().lock(), &mut buf, max_size)?;
    String::from_utf8(buf).map_err(|e| anyhow::anyhow!("stdin is not valid UTF-8: {e}"))
}

fn initial_capacity(max_size: usize) -> usize {
    max_size.min(CHUNK_SIZE)
}

fn read_limited<R: Read>(reader: &mut R, buf: &mut Vec<u8>, max_size: usize) -> anyhow::Result<()> {
    let mut chunk = [0u8; CHUNK_SIZE];
    loop {
        let n = reader.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        let new_len = buf.len().saturating_add(n);
        if new_len > max_size {
            return Err(InputTooLarge::new(max_size, new_len).into());
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn small_file_reads_ok() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(b"hello world").unwrap();
        let out = read_input(Some(file.path().to_path_buf()), 100).unwrap();
        assert_eq!(out, "hello world");
    }

    #[test]
    fn oversized_file_fails_fast_from_metadata() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(&[0u8; 64]).unwrap();
        let err = read_input(Some(file.path().to_path_buf()), 16).unwrap_err();
        assert!(
            err.downcast_ref::<InputTooLarge>().is_some(),
            "expected InputTooLarge, got {err:?}"
        );
    }

    #[test]
    fn read_limited_stops_at_limit() {
        let mut buf = Vec::new();
        let mut cursor = Cursor::new(vec![0u8; 64]);
        let err = read_limited(&mut cursor, &mut buf, 16).unwrap_err();
        assert!(
            err.downcast_ref::<InputTooLarge>().is_some(),
            "expected InputTooLarge, got {err:?}"
        );
        assert!(buf.len() <= 16);
    }

    #[test]
    fn invalid_utf8_file_returns_error() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(&[0xff]).unwrap();
        let err = read_input(Some(file.path().to_path_buf()), 100).unwrap_err();
        assert!(err.to_string().contains("not valid UTF-8"));
    }
}
