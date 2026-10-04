//! Renders tool results and manages binary artifacts and terminal graphics.

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use base64::engine::general_purpose::STANDARD;
use serde::Serialize;
use serde_json::{Map, Value};

use super::{CliError, ImageMode, OutputMode, OutputOptions};
use crate::{
    ToolOutput,
    rust_mcp_schema::{ContentBlock, EmbeddedResourceResource},
};

/// Monotonic component for unique artifact directory names.
static ARTIFACT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Renders one successful tool output in the selected mode.
pub(super) fn render_output(
    options: &OutputOptions,
    output: &ToolOutput,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
    terminal_images: bool,
) -> Result<(), CliError> {
    match options.mode {
        OutputMode::Artifacts => render_artifacts(options, output, stdout, terminal_images),
        OutputMode::Structured => render_structured(output, stdout),
        OutputMode::Raw => render_raw(output, stdout),
        OutputMode::Binary => render_binary(output, stdout, stderr),
    }
}

/// Renders only structured content as compact JSON.
fn render_structured(output: &ToolOutput, stdout: &mut impl Write) -> Result<(), CliError> {
    let structured = output
        .structured_content()
        .ok_or_else(|| CliError::runtime("tool output has no structuredContent"))?;
    write_json(structured, stdout)
}

/// Borrowed successful MCP result used for lossless raw serialization.
#[derive(Serialize)]
struct RawToolResult<'a> {
    /// MCP content blocks.
    content: &'a [ContentBlock],
    /// Successful native handlers always produce a non-error MCP result.
    #[serde(rename = "isError")]
    is_error: bool,
    /// Optional structured payload.
    #[serde(rename = "structuredContent", skip_serializing_if = "Option::is_none")]
    structured_content: Option<&'a Map<String, Value>>,
}

/// Renders the complete successful MCP result as compact JSON.
fn render_raw(output: &ToolOutput, stdout: &mut impl Write) -> Result<(), CliError> {
    let result = RawToolResult {
        content: output.content_blocks(),
        is_error: false,
        structured_content: output.structured_content(),
    };
    write_json(&result, stdout)
}

/// Buffers a complete JSON document before writing it with a trailing newline.
fn write_json(value: &impl Serialize, stdout: &mut impl Write) -> Result<(), CliError> {
    let mut rendered = serde_json::to_vec(value)
        .map_err(|error| CliError::runtime(format!("failed to serialize output: {error}")))?;
    rendered.push(b'\n');
    stdout
        .write_all(&rendered)
        .map_err(|error| CliError::runtime(format!("failed to write stdout: {error}")))
}

/// One borrowed binary content block.
enum BinaryBlock<'a> {
    /// Image data and MIME type.
    Image {
        /// Base64 payload.
        data: &'a str,
        /// Image MIME type.
        mime_type: &'a str,
    },
    /// Audio data and MIME type.
    Audio {
        /// Base64 payload.
        data: &'a str,
        /// Audio MIME type.
        mime_type: &'a str,
    },
    /// Embedded blob data and metadata.
    Blob {
        /// Base64 payload.
        data: &'a str,
        /// Optional blob MIME type.
        mime_type: Option<&'a str>,
    },
}

impl<'a> BinaryBlock<'a> {
    /// Returns the encoded payload.
    fn data(&self) -> &'a str {
        match self {
            Self::Image { data, .. } | Self::Audio { data, .. } | Self::Blob { data, .. } => data,
        }
    }

    /// Returns the optional MIME type.
    fn mime_type(&self) -> Option<&'a str> {
        match self {
            Self::Image { mime_type, .. } | Self::Audio { mime_type, .. } => Some(mime_type),
            Self::Blob { mime_type, .. } => *mime_type,
        }
    }

    /// Returns the controlled artifact filename prefix.
    fn prefix(&self) -> &'static str {
        match self {
            Self::Image { .. } => "image",
            Self::Audio { .. } => "audio",
            Self::Blob { .. } => "blob",
        }
    }
}

/// Collects all binary blocks with their content positions.
fn binary_blocks(output: &ToolOutput) -> Vec<(usize, BinaryBlock<'_>)> {
    output
        .content_blocks()
        .iter()
        .enumerate()
        .filter_map(|(index, block)| match block {
            ContentBlock::ImageContent(image) => Some((
                index,
                BinaryBlock::Image {
                    data: &image.data,
                    mime_type: &image.mime_type,
                },
            )),
            ContentBlock::AudioContent(audio) => Some((
                index,
                BinaryBlock::Audio {
                    data: &audio.data,
                    mime_type: &audio.mime_type,
                },
            )),
            ContentBlock::EmbeddedResource(resource) => match &resource.resource {
                EmbeddedResourceResource::BlobResourceContents(blob) => Some((
                    index,
                    BinaryBlock::Blob {
                        data: &blob.blob,
                        mime_type: blob.mime_type.as_deref(),
                    },
                )),
                EmbeddedResourceResource::TextResourceContents(_) => None,
            },
            ContentBlock::TextContent(_) | ContentBlock::ResourceLink(_) => None,
        })
        .collect()
}

/// Validates every binary payload without retaining decoded copies.
fn validate_binary_blocks(blocks: &[(usize, BinaryBlock<'_>)]) -> Result<(), CliError> {
    for (_, block) in blocks {
        decode_to(block.data(), &mut io::sink()).map_err(|error| {
            CliError::runtime(format!(
                "invalid base64 {} content: {error}",
                block.prefix()
            ))
        })?;
    }
    Ok(())
}

/// Streams one standard base64 payload into a writer.
fn decode_to(data: &str, output: &mut impl Write) -> io::Result<()> {
    let mut input = data.as_bytes();
    let mut decoder = base64::read::DecoderReader::new(&mut input, &STANDARD);
    io::copy(&mut decoder, output).map(|_| ())
}

/// Renders exactly one binary block to stdout and other content to stderr.
fn render_binary(
    output: &ToolOutput,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> Result<(), CliError> {
    let blocks = binary_blocks(output);
    validate_binary_blocks(&blocks)?;
    if blocks.len() != 1 {
        return Err(CliError::runtime(format!(
            "binary output requires exactly one binary content block, found {}",
            blocks.len()
        )));
    }

    let ancillary = render_textual_blocks(output, &BTreeMap::new(), false, true);
    if !ancillary.is_empty() {
        stderr
            .write_all(ancillary.as_bytes())
            .map_err(|error| CliError::runtime(format!("failed to write stderr: {error}")))?;
    }
    decode_to(blocks[0].1.data(), stdout)
        .map_err(|error| CliError::runtime(format!("failed to write binary output: {error}")))
}

/// Renders human-readable output and materializes binary blocks.
fn render_artifacts(
    options: &OutputOptions,
    output: &ToolOutput,
    stdout: &mut impl Write,
    terminal_images: bool,
) -> Result<(), CliError> {
    let blocks = binary_blocks(output);
    validate_binary_blocks(&blocks)?;
    let mut paths = BTreeMap::new();
    let directory = if blocks.is_empty() {
        None
    } else {
        Some(PartialDirectory::create(options.artifact_dir.as_deref())?)
    };

    if let Some(directory) = &directory {
        for (number, (content_index, block)) in blocks.iter().enumerate() {
            let extension = artifact_extension(block.mime_type());
            let filename = format!("{}-{}.{}", block.prefix(), number + 1, extension);
            let path = directory.path.join(filename);
            let mut file = create_private_file(&path)?;
            decode_to(block.data(), &mut file).map_err(|error| {
                CliError::runtime(format!(
                    "failed to write artifact `{}`: {error}",
                    path.display()
                ))
            })?;
            file.flush().map_err(|error| {
                CliError::runtime(format!(
                    "failed to flush artifact `{}`: {error}",
                    path.display()
                ))
            })?;
            paths.insert(*content_index, path);
        }
    }

    let kitty = match options.images {
        ImageMode::Kitty => true,
        ImageMode::Auto => terminal_images,
        ImageMode::Off => false,
    };
    let rendered = render_textual_blocks(output, &paths, kitty, false);
    if let Some(directory) = directory {
        directory.persist();
    }
    stdout
        .write_all(rendered.as_bytes())
        .map_err(|error| CliError::runtime(format!("failed to write stdout: {error}")))
}

/// Renders all non-binary content and optional artifact markers.
fn render_textual_blocks(
    output: &ToolOutput,
    paths: &BTreeMap<usize, PathBuf>,
    kitty: bool,
    omit_binary: bool,
) -> String {
    let mut rendered = Vec::new();
    for (index, block) in output.content_blocks().iter().enumerate() {
        match block {
            ContentBlock::TextContent(text) => rendered.push(text.text.clone()),
            ContentBlock::ImageContent(image) if !omit_binary => {
                let path = paths.get(&index).expect("artifact path exists for image");
                let marker = format!(
                    "[image: {}{}]",
                    path.display(),
                    mime_suffix(Some(&image.mime_type))
                );
                if kitty && image.mime_type == "image/png" {
                    rendered.push(format!("{}\n{marker}", kitty_image(&image.data)));
                } else {
                    rendered.push(marker);
                }
            }
            ContentBlock::AudioContent(audio) if !omit_binary => {
                let path = paths.get(&index).expect("artifact path exists for audio");
                rendered.push(format!(
                    "[audio: {}{}]",
                    path.display(),
                    mime_suffix(Some(&audio.mime_type))
                ));
            }
            ContentBlock::EmbeddedResource(resource) => match &resource.resource {
                EmbeddedResourceResource::TextResourceContents(text) => rendered.push(format!(
                    "[embedded text: {}{}]\n{}",
                    text.uri,
                    mime_suffix(text.mime_type.as_deref()),
                    text.text
                )),
                EmbeddedResourceResource::BlobResourceContents(blob) if !omit_binary => {
                    let path = paths.get(&index).expect("artifact path exists for blob");
                    rendered.push(format!(
                        "[embedded blob: {} -> {}{}]",
                        blob.uri,
                        path.display(),
                        mime_suffix(blob.mime_type.as_deref())
                    ));
                }
                EmbeddedResourceResource::BlobResourceContents(_) => {}
            },
            ContentBlock::ResourceLink(link) => {
                rendered.push(format!("[resource: {} ({})]", link.name, link.uri))
            }
            ContentBlock::ImageContent(_) | ContentBlock::AudioContent(_) => {}
        }
    }
    if let Some(structured) = output.structured_content() {
        rendered.push(
            serde_json::to_string_pretty(structured)
                .expect("JSON values always serialize successfully"),
        );
    }
    if rendered.is_empty() {
        String::new()
    } else {
        format!("{}\n", rendered.join("\n\n"))
    }
}

/// Returns a MIME marker suffix.
fn mime_suffix(mime_type: Option<&str>) -> String {
    mime_type
        .map(|mime_type| format!(" ({mime_type})"))
        .unwrap_or_default()
}

/// Returns a controlled extension for a content MIME type.
fn artifact_extension(mime_type: Option<&str>) -> &'static str {
    match mime_type {
        Some("image/png") => "png",
        Some("image/jpeg") => "jpg",
        Some("image/gif") => "gif",
        Some("image/webp") => "webp",
        Some("audio/wav") | Some("audio/x-wav") => "wav",
        Some("audio/mpeg") => "mp3",
        Some("audio/ogg") => "ogg",
        Some("application/pdf") => "pdf",
        _ => "bin",
    }
}

/// Encodes a PNG payload as chunked Kitty graphics protocol commands.
fn kitty_image(data: &str) -> String {
    let chunks = data.as_bytes().chunks(4096);
    let chunk_count = chunks.len();
    let mut rendered = String::new();
    if chunk_count == 0 {
        return "\u{1b}_Ga=T,f=100,m=0;\u{1b}\\".into();
    }
    for (index, chunk) in chunks.enumerate() {
        let more = usize::from(index + 1 < chunk_count);
        if index == 0 {
            rendered.push_str(&format!("\u{1b}_Ga=T,f=100,m={more};"));
        } else {
            rendered.push_str(&format!("\u{1b}_Gm={more};"));
        }
        rendered.push_str(std::str::from_utf8(chunk).expect("base64 is ASCII"));
        rendered.push_str("\u{1b}\\");
    }
    rendered
}

/// Returns whether environment hints identify a Kitty-compatible terminal.
pub(super) fn kitty_terminal() -> bool {
    std::env::var_os("KITTY_WINDOW_ID").is_some()
        || std::env::var("TERM").is_ok_and(|term| term.contains("kitty"))
}

/// Removes a partially written invocation directory unless persisted.
struct PartialDirectory {
    /// Unique invocation path.
    path: PathBuf,
    /// Whether successful artifacts should remain.
    persisted: bool,
}

impl PartialDirectory {
    /// Creates a private unique directory beneath the selected parent.
    fn create(parent: Option<&Path>) -> Result<Self, CliError> {
        let parent = resolve_artifact_parent(parent)?;
        fs::create_dir_all(&parent).map_err(|error| {
            CliError::runtime(format!(
                "failed to create artifact parent `{}`: {error}",
                parent.display()
            ))
        })?;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        for _ in 0..100 {
            let sequence = ARTIFACT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!(
                "mercutio-{}-{timestamp}-{sequence}",
                std::process::id()
            ));
            match create_private_directory(&path) {
                Ok(()) => {
                    return Ok(Self {
                        path,
                        persisted: false,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(CliError::runtime(format!(
                        "failed to create artifact directory `{}`: {error}",
                        path.display()
                    )));
                }
            }
        }
        Err(CliError::runtime(
            "failed to choose a unique artifact directory",
        ))
    }

    /// Keeps this directory after the renderer returns.
    fn persist(mut self) {
        self.persisted = true;
    }
}

impl Drop for PartialDirectory {
    fn drop(&mut self) {
        if !self.persisted {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

/// Resolves an artifact parent to an absolute path.
fn resolve_artifact_parent(parent: Option<&Path>) -> Result<PathBuf, CliError> {
    let parent = parent.map_or_else(std::env::temp_dir, Path::to_path_buf);
    if parent.is_absolute() {
        Ok(parent)
    } else {
        std::env::current_dir()
            .map(|current| current.join(parent))
            .map_err(|error| {
                CliError::runtime(format!("failed to resolve current directory: {error}"))
            })
    }
}

/// Creates a private invocation directory where supported.
fn create_private_directory(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    builder.mode(0o700);
    builder.recursive(false).create(path)
}

/// Creates a private artifact file where supported.
fn create_private_file(path: &Path) -> Result<File, CliError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    options.open(path).map_err(|error| {
        CliError::runtime(format!(
            "failed to create artifact `{}`: {error}",
            path.display()
        ))
    })
}
