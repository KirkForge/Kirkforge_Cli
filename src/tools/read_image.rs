//! `read_image` — attach a local image to the conversation.
//!
//! Reads the file at the given path, base64-encodes the bytes, and
//! returns them as a [`ToolOutcome::Image`]. The executor's
//! `handle_tool_outcome` materialises that into a `Role::Tool` message
//! with `content_parts: [Image{…}]`; the next user-prompt turn then
//! has the image spliced onto the user message by
//! `PromptBuilder::build_messages`, so the model sees it as part of
//! the user's question (the same shape as if the user had pasted the
//! image directly).
//!
//! # Why a tool, not a CLI flag or input-box paste
//!
//! A `!`-prefixed "read screenshot" command would require either an
//! out-of-band event channel or a separate command. A paste handler
//! in the TUI input box would require a base64-encoding layer in the
//! UI hot path. A tool is the same plumbing the model already uses
//! for `read_file`, and the model can compose it ("look at the diff
//! in the screenshot and tell me what you see" — the model calls
//! `read_image` with the path the user gave it).
//!
//! # Mime detection
//!
//! MIME type is detected from the file's magic bytes, not the
//! extension. This handles mis-named files and lets the tool
//! support common formats without guessing. The `error` arm returns
//! a clear "unsupported format" message rather than silently
//! sending the wrong MIME to the model.
//!
//! # Size cap
//!
//! No explicit cap; the adapter layer doesn't enforce one either.
//! Practical limit is whatever the model server accepts (OpenAI
//! vision caps at ~20 MB; Ollama native has no documented cap).
//! `Config::max_file_read_size` applies to text files only, not
//! binary — the tool reads the raw bytes regardless.
use crate::shared::{ToolDef, ToolOutcome};
use crate::tools::Tool;
use std::path::PathBuf;

pub struct ReadImage;

/// Detect an image MIME type from the leading magic bytes of `bytes`.
///
/// A leading UTF-8 BOM and ASCII whitespace are skipped before checking
/// signatures, so pretty-printed XML and files with spurious leading
/// bytes are handled consistently.
///
/// Supported signatures:
///   PNG   — `89 50 4E 47 0D 0A 1A 0A`
///   JPEG  — `FF D8 FF`
///   GIF   — `47 49 46 38 37 61` or `47 49 46 38 39 61`
///   WEBP  — `52 49 46 ?? ?? 57 45 42 50` (RIFF container with WEBP tag)
///   BMP   — `42 4D`
///   SVG   — `<?xml` or `<svg` after the optional BOM/whitespace skip
fn mime_for_bytes(bytes: &[u8]) -> Option<&'static str> {
    // Skip a leading UTF-8 BOM and any ASCII whitespace before checking
    // signatures. This makes detection forgiving of BOM-prefixed or
    // whitespace-prefixed files without weakening the magic-byte checks.
    let mut i = 0;
    let bom: [u8; 3] = [0xEF, 0xBB, 0xBF];
    if bytes.starts_with(&bom) {
        i += 3;
    }
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    let rest = &bytes[i..];

    if rest.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if rest.len() >= 3 && rest[0] == 0xFF && rest[1] == 0xD8 && rest[2] == 0xFF {
        Some("image/jpeg")
    } else if rest.starts_with(b"GIF87a") || rest.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if rest.len() >= 12 && rest.starts_with(b"RIFF") && &rest[8..12] == b"WEBP" {
        Some("image/webp")
    } else if rest.starts_with(b"BM") {
        Some("image/bmp")
    } else if rest.starts_with(b"<?xml") || rest.starts_with(b"<svg") {
        Some("image/svg+xml")
    } else {
        None
    }
}

#[async_trait::async_trait]
impl Tool for ReadImage {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "read_image",
            description: "Read an image file from disk and attach it to the conversation as a multimodal part. The model will see the image on the *next* user turn — call this tool first, then ask the model about the image. Supported formats: png, jpg, jpeg, gif, webp, bmp, svg.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the image file (relative to project root or absolute)"
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn run(&self, args: serde_json::Value) -> ToolOutcome {
        let path = match args.get("path").and_then(|p| p.as_str()) {
            Some(p) => PathBuf::from(shellexpand::tilde(p).as_ref()),
            None => {
                return ToolOutcome::Error {
                    message: "Missing 'path' argument".into(),
                }
            }
        };

        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                return ToolOutcome::Error {
                    message: format!("Cannot read {}: {}", path.display(), e),
                }
            }
        };

        let mime = match mime_for_bytes(&bytes) {
            Some(m) => m,
            None => {
                return ToolOutcome::Error {
                    message: format!(
                        "Unsupported image format ({}). Supported: png, jpg, jpeg, gif, webp, bmp, svg.",
                        path.display()
                    ),
                }
            }
        };

        // `base64::engine::general_purpose::STANDARD` is the alphabet
        // OpenAI vision and Ollama both expect (`+`, `/`, `=` padding).
        // `Engine::encode` takes `&[u8]`.
        use base64::Engine;
        let data_base64 = base64::engine::general_purpose::STANDARD.encode(&bytes);

        ToolOutcome::Image {
            path,
            mime: mime.to_string(),
            data_base64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Tool;

    const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

    fn make_args(path: &str) -> serde_json::Value {
        serde_json::json!({"path": path})
    }

    #[tokio::test]
    async fn read_image_png_returns_base64_with_correct_mime() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("shot.png");
        std::fs::write(&p, PNG_MAGIC).unwrap();

        let out = ReadImage.run(make_args(&p.display().to_string())).await;
        match out {
            ToolOutcome::Image {
                path,
                mime,
                data_base64,
            } => {
                assert_eq!(path, p);
                assert_eq!(mime, "image/png");
                // base64 of the 8-byte PNG magic
                assert_eq!(data_base64, "iVBORw0KGgo=");
            }
            other => panic!("expected Image outcome, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn read_image_jpeg_returns_image_jpeg_mime() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("pic.jpg");
        // SOI marker for JPEG
        std::fs::write(&p, [0xFF, 0xD8, 0xFF]).unwrap();

        let out = ReadImage.run(make_args(&p.display().to_string())).await;
        assert!(matches!(out, ToolOutcome::Image { ref mime, .. } if mime == "image/jpeg"));
    }

    #[tokio::test]
    async fn read_image_unknown_extension_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("mystery.xyz");
        std::fs::write(&p, b"some bytes").unwrap();

        let out = ReadImage.run(make_args(&p.display().to_string())).await;
        assert!(matches!(out, ToolOutcome::Error { .. }));
    }

    #[tokio::test]
    async fn read_image_misnamed_jpeg_is_detected_by_magic_bytes() {
        let dir = tempfile::tempdir().unwrap();
        // Wrong extension: .png, but the bytes are JPEG magic.
        let p = dir.path().join("actually_jpeg.png");
        std::fs::write(&p, [0xFF, 0xD8, 0xFF, 0xE0]).unwrap();

        let out = ReadImage.run(make_args(&p.display().to_string())).await;
        assert!(matches!(out, ToolOutcome::Image { ref mime, .. } if mime == "image/jpeg"));
    }

    #[tokio::test]
    async fn read_image_gif_by_magic_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("anim.gif");
        std::fs::write(&p, b"GIF89a\x01\x00").unwrap();

        let out = ReadImage.run(make_args(&p.display().to_string())).await;
        assert!(matches!(out, ToolOutcome::Image { ref mime, .. } if mime == "image/gif"));
    }

    #[tokio::test]
    async fn read_image_webp_by_magic_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("pic.webp");
        // Minimal RIFF/WEBP header: RIFF + 4-byte size + WEBP.
        let bytes: Vec<u8> = b"RIFF"
            .iter()
            .copied()
            .chain([0x00, 0x00, 0x00, 0x00].iter().copied())
            .chain(b"WEBP".iter().copied())
            .collect();
        std::fs::write(&p, bytes).unwrap();

        let out = ReadImage.run(make_args(&p.display().to_string())).await;
        assert!(matches!(out, ToolOutcome::Image { ref mime, .. } if mime == "image/webp"));
    }

    #[tokio::test]
    async fn read_image_bmp_by_magic_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("shot.bmp");
        std::fs::write(&p, b"BM\x00\x00\x00\x00").unwrap();

        let out = ReadImage.run(make_args(&p.display().to_string())).await;
        assert!(matches!(out, ToolOutcome::Image { ref mime, .. } if mime == "image/bmp"));
    }

    #[tokio::test]
    async fn read_image_svg_by_magic_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("icon.svg");
        std::fs::write(&p, b"<?xml version=\"1.0\"?><svg></svg>").unwrap();

        let out = ReadImage.run(make_args(&p.display().to_string())).await;
        assert!(matches!(out, ToolOutcome::Image { ref mime, .. } if mime == "image/svg+xml"));
    }

    #[tokio::test]
    async fn read_image_svg_with_bom_and_whitespace() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bom.svg");
        let bytes: Vec<u8> = [0xEF, 0xBB, 0xBF]
            .iter()
            .copied()
            .chain(b"\n  \t<svg>".iter().copied())
            .collect();
        std::fs::write(&p, bytes).unwrap();

        let out = ReadImage.run(make_args(&p.display().to_string())).await;
        assert!(matches!(out, ToolOutcome::Image { ref mime, .. } if mime == "image/svg+xml"));
    }

    #[tokio::test]
    async fn read_image_png_with_bom_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bom.png");
        let bytes: Vec<u8> = [0xEF, 0xBB, 0xBF]
            .iter()
            .copied()
            .chain(PNG_MAGIC.iter().copied())
            .collect();
        std::fs::write(&p, bytes).unwrap();

        let out = ReadImage.run(make_args(&p.display().to_string())).await;
        assert!(matches!(out, ToolOutcome::Image { ref mime, .. } if mime == "image/png"));
    }

    #[tokio::test]
    async fn read_image_jpeg_with_whitespace_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("space.jpg");
        let bytes: Vec<u8> = b"\n  \t"
            .iter()
            .copied()
            .chain([0xFF, 0xD8, 0xFF].iter().copied())
            .collect();
        std::fs::write(&p, bytes).unwrap();

        let out = ReadImage.run(make_args(&p.display().to_string())).await;
        assert!(matches!(out, ToolOutcome::Image { ref mime, .. } if mime == "image/jpeg"));
    }

    #[tokio::test]
    async fn read_image_gif_with_bom_and_whitespace_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bom.gif");
        let bytes: Vec<u8> = [0xEF, 0xBB, 0xBF]
            .iter()
            .copied()
            .chain(b"\n".iter().copied())
            .chain(b"GIF89a\x01\x00".iter().copied())
            .collect();
        std::fs::write(&p, bytes).unwrap();

        let out = ReadImage.run(make_args(&p.display().to_string())).await;
        assert!(matches!(out, ToolOutcome::Image { ref mime, .. } if mime == "image/gif"));
    }

    #[tokio::test]
    async fn read_image_missing_path_returns_error() {
        let out = ReadImage.run(serde_json::json!({})).await;
        assert!(matches!(out, ToolOutcome::Error { .. }));
    }

    #[tokio::test]
    async fn read_image_nonexistent_file_returns_error() {
        let out = ReadImage
            .run(make_args("/nonexistent/path/to/file.png"))
            .await;
        assert!(matches!(out, ToolOutcome::Error { .. }));
    }
}
