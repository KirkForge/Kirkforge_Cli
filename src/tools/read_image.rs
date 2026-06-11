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
//! We detect the mime type from the file extension rather than
//! sniffing the bytes. The four supported formats cover >99% of
//! screenshots in practice; the `error` arm returns a clear
//! "unsupported format" message rather than silently sending the
//! wrong mime to the model. (Sniffing the magic bytes would be
//! more robust but adds a `mime_guess` dependency for marginal
//! benefit; revisit if the use case broadens.)
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

fn mime_for(path: &std::path::Path) -> Option<&'static str> {
    match path.extension().and_then(|s| s.to_str()) {
        Some(ext) => match ext.to_ascii_lowercase().as_str() {
            "png" => Some("image/png"),
            "jpg" | "jpeg" => Some("image/jpeg"),
            "gif" => Some("image/gif"),
            "webp" => Some("image/webp"),
            _ => None,
        },
        None => None,
    }
}

#[async_trait::async_trait]
impl Tool for ReadImage {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "read_image",
            description: "Read an image file from disk and attach it to the conversation as a multimodal part. The model will see the image on the *next* user turn — call this tool first, then ask the model about the image. Supported formats: png, jpg, jpeg, gif, webp.",
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

        let mime = match mime_for(&path) {
            Some(m) => m,
            None => {
                return ToolOutcome::Error {
                    message: format!(
                        "Unsupported image format ({}). Supported: png, jpg, jpeg, gif, webp.",
                        path.display()
                    ),
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
        std::fs::write(&p, &PNG_MAGIC).unwrap();

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
        std::fs::write(&p, &[0xFF, 0xD8, 0xFF]).unwrap();

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
