//! Host-owned image assets used by prompt delivery and lazy history loading.
//!
//! Pi remains the source of truth for conversation history.  This store is a
//! derived, host-local representation: an uploaded image is written once
//! under the Pix configuration directory and a historical Pi `ImageContent`
//! can be externalized to the same layout when a client opts into
//! `image_refs.v1`.

use std::fs::{self, File};
use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::Builder;
use thiserror::Error;

use crate::session_lock::SessionId;

/// Maximum decoded image size accepted by the host asset pipeline.
///
/// The wire layer is already bounded to 1 MiB per encrypted frame and the
/// Pi RPC layer has its own 16 MiB record limit.  This larger bound protects
/// the durable asset path when importing an existing Pi history.
pub const MAX_IMAGE_ASSET_BYTES: usize = 16 * 1024 * 1024;
/// Target maximum dimension for the model-facing derivative.
pub const MAX_VISION_DIMENSION: u32 = 2000;
const ASSET_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageAsset {
    pub id: String,
    pub mime_type: String,
    pub size: u64,
    pub source_width: Option<u32>,
    pub source_height: Option<u32>,
    pub vision_width: Option<u32>,
    pub vision_height: Option<u32>,
    pub source_path: PathBuf,
    pub agent_path: PathBuf,
    pub vision_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageAssetChunk {
    pub id: String,
    pub mime_type: String,
    pub offset: u64,
    pub total_size: u64,
    pub eof: bool,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct ImageAssetStore {
    root: PathBuf,
}

impl ImageAssetStore {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Persists one source image using a content-addressed, session-scoped
    /// directory.  Every file is created through a same-directory temporary
    /// file and an atomic rename; existing hashes are reused.
    ///
    /// # Errors
    ///
    /// Returns [`ImageAssetError`] when the bytes, MIME type, or filesystem
    /// operation is invalid.
    pub fn persist(
        &self,
        session_id: SessionId,
        mime_type: impl Into<String>,
        bytes: &[u8],
    ) -> Result<ImageAsset, ImageAssetError> {
        let mime_type = mime_type.into();
        validate_mime(&mime_type)?;
        if bytes.is_empty() || bytes.len() > MAX_IMAGE_ASSET_BYTES {
            return Err(ImageAssetError::Size(bytes.len()));
        }
        let vision = derive_vision(bytes, &mime_type).0;
        let vision_hex = hex_digest(&Sha256::digest(&vision));
        self.persist_with_key(session_id, &vision_hex, mime_type, bytes, Some(vision))
    }

    /// Persists an uploaded image under the client-provided attachment key
    /// while retaining the content-addressed vision reference in metadata.
    /// The key is never used as a filesystem path without validation.
    ///
    /// # Errors
    ///
    /// Returns [`ImageAssetError`] when the key, MIME type, image bytes, or
    /// filesystem operation is invalid.
    pub fn persist_named(
        &self,
        session_id: SessionId,
        attachment_id: &str,
        mime_type: impl Into<String>,
        bytes: &[u8],
    ) -> Result<ImageAsset, ImageAssetError> {
        validate_asset_key(attachment_id)?;
        self.persist_with_key(session_id, attachment_id, mime_type.into(), bytes, None)
    }

    fn persist_with_key(
        &self,
        session_id: SessionId,
        asset_key: &str,
        mime_type: String,
        bytes: &[u8],
        known_vision: Option<Vec<u8>>,
    ) -> Result<ImageAsset, ImageAssetError> {
        validate_mime(&mime_type)?;
        if bytes.is_empty() || bytes.len() > MAX_IMAGE_ASSET_BYTES {
            return Err(ImageAssetError::Size(bytes.len()));
        }
        let source_digest = Sha256::digest(bytes);
        let source_hex = hex_digest(&source_digest);
        let source_dimensions = image_dimensions(bytes);
        let (vision_bytes, vision_is_source_fallback) = match known_vision {
            Some(vision) => {
                let fallback = vision == bytes;
                (vision, fallback)
            }
            None => derive_vision(bytes, &mime_type),
        };
        if vision_bytes.len() > MAX_IMAGE_ASSET_BYTES {
            return Err(ImageAssetError::Size(vision_bytes.len()));
        }
        let vision_digest = Sha256::digest(&vision_bytes);
        let vision_hex = hex_digest(&vision_digest);
        let vision_dimensions = image_dimensions(&vision_bytes);
        let id = format!("sha256:{vision_hex}");
        let directory = self
            .root
            .join("v1")
            .join(session_id.to_string())
            .join(asset_key);
        fs::create_dir_all(&directory).map_err(|source| ImageAssetError::Io {
            path: directory.clone(),
            source,
        })?;
        set_private_directory(&directory)?;

        let source_path = directory.join("source");
        let agent_path = directory.join("agent");
        let vision_path = directory.join("vision");
        write_atomic_if_missing(&source_path, bytes)?;
        link_or_copy(&source_path, &agent_path)?;
        write_atomic_if_missing(&vision_path, &vision_bytes)?;

        let metadata_path = directory.join("metadata.json");
        if !metadata_path.exists() {
            let metadata = json!({
                "version": ASSET_VERSION,
                "attachmentId": asset_key,
                "mimeType": mime_type,
                "size": vision_bytes.len(),
                "sourceSize": bytes.len(),
                "sourceSha256": format!("sha256:{source_hex}"),
                "sourceWidth": source_dimensions.map(|(width, _)| width),
                "sourceHeight": source_dimensions.map(|(_, height)| height),
                "pixelWidth": source_dimensions.map(|(width, _)| width),
                "pixelHeight": source_dimensions.map(|(_, height)| height),
                "sourcePath": source_path,
                "agentPath": agent_path,
                "visionPath": vision_path,
                "visionSha256": id,
                "visionWidth": vision_dimensions.map(|(width, _)| width),
                "visionHeight": vision_dimensions.map(|(_, height)| height),
                "visionMaxDimension": MAX_VISION_DIMENSION,
                "visionIsSourceFallback": vision_is_source_fallback,
            });
            let encoded = serde_json::to_vec(&metadata).map_err(ImageAssetError::Encode)?;
            write_atomic(&metadata_path, &encoded)?;
        }

        Ok(ImageAsset {
            id,
            mime_type,
            size: vision_bytes.len() as u64,
            source_width: source_dimensions.map(|(width, _)| width),
            source_height: source_dimensions.map(|(_, height)| height),
            vision_width: vision_dimensions.map(|(width, _)| width),
            vision_height: vision_dimensions.map(|(_, height)| height),
            source_path,
            agent_path,
            vision_path,
        })
    }

    /// Replaces Pi image content in-place with small lazy-load references.
    /// Text and all non-image message fields remain untouched.
    ///
    /// # Errors
    ///
    /// Returns [`ImageAssetError`] when an image part is malformed or cannot
    /// be persisted atomically.
    pub fn externalize_messages(
        &self,
        session_id: SessionId,
        messages: &mut [Value],
    ) -> Result<(), ImageAssetError> {
        for message in messages {
            self.externalize_value(session_id, message)?;
        }
        Ok(())
    }

    /// Reads a bounded range of a vision asset for a lazy history request.
    /// The caller must have already authorized `session_id`.
    ///
    /// # Errors
    ///
    /// Returns [`ImageAssetError`] when the reference, metadata, range, or
    /// backing file is invalid or unavailable.
    pub fn read_chunk(
        &self,
        session_id: SessionId,
        image_ref: &str,
        offset: u64,
        limit: usize,
    ) -> Result<ImageAssetChunk, ImageAssetError> {
        let hex = parse_image_ref(image_ref)?;
        if limit == 0 {
            return Err(ImageAssetError::InvalidReference);
        }
        let directory = self.find_directory(session_id, &hex)?;
        let vision_path = directory.join("vision");
        let metadata_path = directory.join("metadata.json");
        let metadata = fs::read(&metadata_path).map_err(|source| ImageAssetError::Io {
            path: metadata_path.clone(),
            source,
        })?;
        let metadata: Value = serde_json::from_slice(&metadata).map_err(ImageAssetError::Decode)?;
        let mime_type = metadata
            .get("mimeType")
            .and_then(Value::as_str)
            .ok_or(ImageAssetError::InvalidMetadata("mimeType"))?
            .to_owned();
        let mut file = File::open(&vision_path).map_err(|source| ImageAssetError::Io {
            path: vision_path.clone(),
            source,
        })?;
        let total_size = file
            .metadata()
            .map_err(|source| ImageAssetError::Io {
                path: vision_path.clone(),
                source,
            })?
            .len();
        if offset > total_size {
            return Err(ImageAssetError::Range {
                offset,
                total: total_size,
            });
        }
        file.seek(SeekFrom::Start(offset))
            .map_err(|source| ImageAssetError::Io {
                path: vision_path,
                source,
            })?;
        let remaining = total_size.saturating_sub(offset);
        let read_size = usize::try_from(remaining.min(u64::try_from(limit).unwrap_or(u64::MAX)))
            .unwrap_or(limit);
        let mut data = vec![0_u8; read_size];
        file.read_exact(&mut data)
            .map_err(|source| ImageAssetError::Io {
                path: directory.join("vision"),
                source,
            })?;
        Ok(ImageAssetChunk {
            id: image_ref.to_owned(),
            mime_type,
            offset,
            total_size,
            eof: offset.saturating_add(read_size as u64) >= total_size,
            data,
        })
    }

    fn find_directory(
        &self,
        session_id: SessionId,
        vision_hex: &str,
    ) -> Result<PathBuf, ImageAssetError> {
        let session_directory = self.root.join("v1").join(session_id.to_string());
        let direct = session_directory.join(vision_hex);
        let expected = format!("sha256:{vision_hex}");
        if let Ok(metadata) = fs::read(direct.join("metadata.json"))
            && let Ok(metadata) = serde_json::from_slice::<Value>(&metadata)
            && metadata.get("visionSha256").and_then(Value::as_str) == Some(expected.as_str())
        {
            return Ok(direct);
        }
        let entries = fs::read_dir(&session_directory).map_err(|source| ImageAssetError::Io {
            path: session_directory.clone(),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| ImageAssetError::Io {
                path: session_directory.clone(),
                source,
            })?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let metadata_path = path.join("metadata.json");
            let Ok(metadata) = fs::read(&metadata_path) else {
                continue;
            };
            let Ok(metadata) = serde_json::from_slice::<Value>(&metadata) else {
                continue;
            };
            if metadata.get("visionSha256").and_then(Value::as_str) == Some(expected.as_str()) {
                return Ok(path);
            }
        }
        Err(ImageAssetError::Io {
            path: session_directory,
            source: io::Error::new(io::ErrorKind::NotFound, "image asset was not found"),
        })
    }

    fn externalize_value(
        &self,
        session_id: SessionId,
        value: &mut Value,
    ) -> Result<(), ImageAssetError> {
        match value {
            Value::Array(values) => {
                for value in values {
                    self.externalize_value(session_id, value)?;
                }
            }
            Value::Object(object) => {
                let image = object.get("type").and_then(Value::as_str) == Some("image");
                if image {
                    let data = object
                        .get("data")
                        .and_then(Value::as_str)
                        .ok_or(ImageAssetError::InvalidImage("missing image data"))?
                        .to_owned();
                    let mime_type = object
                        .get("mimeType")
                        .and_then(Value::as_str)
                        .ok_or(ImageAssetError::InvalidImage("missing image MIME type"))?
                        .to_owned();
                    let decoded = STANDARD
                        .decode(data)
                        .map_err(|_| ImageAssetError::InvalidImage("invalid image base64"))?;
                    let asset = self.persist(session_id, mime_type, &decoded)?;
                    object.clear();
                    object.insert("type".to_owned(), Value::String("imageRef".to_owned()));
                    object.insert("id".to_owned(), Value::String(asset.id));
                    object.insert("mimeType".to_owned(), Value::String(asset.mime_type));
                    object.insert("size".to_owned(), Value::from(asset.size));
                    if let Some(width) = asset.source_width.or(asset.vision_width) {
                        object.insert("pixelWidth".to_owned(), Value::from(width));
                    }
                    if let Some(height) = asset.source_height.or(asset.vision_height) {
                        object.insert("pixelHeight".to_owned(), Value::from(height));
                    }
                } else {
                    for value in object.values_mut() {
                        self.externalize_value(session_id, value)?;
                    }
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
        Ok(())
    }
}

fn validate_mime(mime_type: &str) -> Result<(), ImageAssetError> {
    if matches!(
        mime_type,
        "image/png" | "image/jpeg" | "image/webp" | "image/gif"
    ) {
        Ok(())
    } else {
        Err(ImageAssetError::UnsupportedMime(mime_type.to_owned()))
    }
}

/// Produces the model-facing image variant. Unsupported or malformed image
/// bytes are retained verbatim: Pi already accepts the declared MIME/data and
/// preserving them is safer than silently dropping a historical message.
fn derive_vision(bytes: &[u8], mime_type: &str) -> (Vec<u8>, bool) {
    let Ok(reader) = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format() else {
        return (bytes.to_vec(), true);
    };
    let Ok(image) = reader.decode() else {
        return (bytes.to_vec(), true);
    };
    if image.width() <= MAX_VISION_DIMENSION && image.height() <= MAX_VISION_DIMENSION {
        return (bytes.to_vec(), false);
    }
    let resized = image.resize(
        MAX_VISION_DIMENSION,
        MAX_VISION_DIMENSION,
        image::imageops::FilterType::Triangle,
    );
    let format = match mime_type {
        "image/png" => image::ImageFormat::Png,
        "image/jpeg" => image::ImageFormat::Jpeg,
        "image/webp" => image::ImageFormat::WebP,
        "image/gif" => image::ImageFormat::Gif,
        _ => return (bytes.to_vec(), true),
    };
    let mut output = Cursor::new(Vec::new());
    if resized.write_to(&mut output, format).is_err() {
        return (bytes.to_vec(), true);
    }
    (output.into_inner(), false)
}

fn image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()
        .map(|image| (image.width(), image.height()))
}

fn parse_image_ref(image_ref: &str) -> Result<String, ImageAssetError> {
    let hex = image_ref
        .strip_prefix("sha256:")
        .ok_or(ImageAssetError::InvalidReference)?;
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ImageAssetError::InvalidReference);
    }
    Ok(hex.to_ascii_lowercase())
}

fn validate_asset_key(value: &str) -> Result<(), ImageAssetError> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(ImageAssetError::InvalidAssetKey);
    }
    Ok(())
}

fn hex_digest(digest: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}

fn write_atomic_if_missing(path: &Path, bytes: &[u8]) -> Result<(), ImageAssetError> {
    if path.exists() {
        return Ok(());
    }
    write_atomic(path, bytes)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), ImageAssetError> {
    let parent = path
        .parent()
        .ok_or_else(|| ImageAssetError::InvalidPath(path.to_path_buf()))?;
    fs::create_dir_all(parent).map_err(|source| ImageAssetError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    let mut temporary = Builder::new()
        .prefix(".pix-image-")
        .tempfile_in(parent)
        .map_err(|source| ImageAssetError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    set_private_file(temporary.as_file())?;
    temporary
        .write_all(bytes)
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|source| ImageAssetError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    match temporary.persist(path) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(ImageAssetError::Io {
            path: path.to_path_buf(),
            source: error.error,
        }),
    }
}

fn link_or_copy(source: &Path, destination: &Path) -> Result<(), ImageAssetError> {
    if destination.exists() {
        return Ok(());
    }
    if fs::hard_link(source, destination).is_err() {
        let bytes = fs::read(source).map_err(|source_error| ImageAssetError::Io {
            path: source.to_path_buf(),
            source: source_error,
        })?;
        write_atomic(destination, &bytes)?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_private_directory(path: &Path) -> Result<(), ImageAssetError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
        ImageAssetError::Io {
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn set_private_directory(_path: &Path) -> Result<(), ImageAssetError> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file(file: &File) -> Result<(), ImageAssetError> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|source| ImageAssetError::Io {
            path: PathBuf::from("<temporary image asset>"),
            source,
        })
}

#[cfg(not(unix))]
fn set_private_file(_file: &File) -> Result<(), ImageAssetError> {
    Ok(())
}

#[derive(Debug, Error)]
pub enum ImageAssetError {
    #[error("unsupported image MIME type: {0}")]
    UnsupportedMime(String),
    #[error("image size {0} is outside the supported range")]
    Size(usize),
    #[error("invalid image reference")]
    InvalidReference,
    #[error("invalid image asset key")]
    InvalidAssetKey,
    #[error("invalid image content: {0}")]
    InvalidImage(&'static str),
    #[error("invalid image metadata field: {0}")]
    InvalidMetadata(&'static str),
    #[error("image range starts at {offset}, beyond {total} bytes")]
    Range { offset: u64, total: u64 },
    #[error("invalid image asset path: {0}")]
    InvalidPath(PathBuf),
    #[error("image asset I/O at {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("failed to encode image metadata: {0}")]
    Encode(serde_json::Error),
    #[error("failed to decode image metadata: {0}")]
    Decode(serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::{ImageAssetStore, MAX_VISION_DIMENSION};
    use crate::session_lock::SessionId;
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde_json::json;
    use tempfile::tempdir;

    #[test]
    fn persists_and_externalizes_image_content_atomically() {
        let directory = tempdir().expect("directory");
        let store = ImageAssetStore::new(directory.path());
        let session = SessionId::new();
        let bytes = b"image bytes";
        let mut messages = vec![json!({
            "role": "user",
            "content": [{"type": "image", "mimeType": "image/png", "data": STANDARD.encode(bytes)}]
        })];
        store
            .externalize_messages(session, &mut messages)
            .expect("externalize");
        let reference = &messages[0]["content"][0];
        assert_eq!(reference["type"], "imageRef");
        assert!(
            reference["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("sha256:"))
        );
        assert_eq!(reference["size"], bytes.len());
        assert!(directory.path().join("v1").exists());
        assert_eq!(MAX_VISION_DIMENSION, 2000);
    }

    #[test]
    fn image_references_include_original_pixel_dimensions() {
        let directory = tempdir().expect("directory");
        let store = ImageAssetStore::new(directory.path());
        let session = SessionId::new();
        let image = image::RgbImage::new(4, 2);
        let mut encoded = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image)
            .write_to(&mut encoded, image::ImageFormat::Png)
            .expect("encode image");

        let asset = store
            .persist(session, "image/png", encoded.get_ref())
            .expect("persist");
        assert_eq!(asset.source_width, Some(4));
        assert_eq!(asset.source_height, Some(2));

        let mut messages = vec![json!({
            "role": "user",
            "content": [{
                "type": "image",
                "mimeType": "image/png",
                "data": STANDARD.encode(encoded.get_ref())
            }]
        })];
        store
            .externalize_messages(session, &mut messages)
            .expect("externalize");
        let reference = &messages[0]["content"][0];
        assert_eq!(reference["pixelWidth"], 4);
        assert_eq!(reference["pixelHeight"], 2);

        let metadata: serde_json::Value = serde_json::from_slice(
            &std::fs::read(asset.source_path.parent().unwrap().join("metadata.json"))
                .expect("metadata"),
        )
        .expect("decode metadata");
        assert_eq!(metadata["pixelWidth"], 4);
        assert_eq!(metadata["pixelHeight"], 2);
    }

    #[test]
    fn reads_lazy_chunks_with_range_bounds() {
        let directory = tempdir().expect("directory");
        let store = ImageAssetStore::new(directory.path());
        let session = SessionId::new();
        let asset = store
            .persist(session, "image/jpeg", b"abcdef")
            .expect("persist");
        let chunk = store.read_chunk(session, &asset.id, 2, 3).expect("chunk");
        assert_eq!(chunk.data, b"cde");
        assert!(!chunk.eof);
        let last = store
            .read_chunk(session, &asset.id, 5, 3)
            .expect("last chunk");
        assert_eq!(last.data, b"f");
        assert!(last.eof);
    }

    #[test]
    fn large_valid_images_get_a_bounded_vision_variant() {
        let directory = tempdir().expect("directory");
        let store = ImageAssetStore::new(directory.path());
        let session = SessionId::new();
        let image = image::RgbImage::new(2_001, 1);
        let mut encoded = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image)
            .write_to(&mut encoded, image::ImageFormat::Png)
            .expect("encode image");
        let asset = store
            .persist(session, "image/png", encoded.get_ref())
            .expect("persist");
        let vision_bytes = std::fs::read(&asset.vision_path).expect("vision bytes");
        let decoded = image::ImageReader::new(std::io::Cursor::new(vision_bytes))
            .with_guessed_format()
            .expect("format")
            .decode()
            .expect("dimensions");
        let (width, height) = (decoded.width(), decoded.height());
        assert!(width <= MAX_VISION_DIMENSION);
        assert!(height <= MAX_VISION_DIMENSION);
    }
}
