//! Typed image-generation requests and items.

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD as BASE64_STANDARD, URL_SAFE, URL_SAFE_NO_PAD};
use serde::Deserialize;
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use std::path::{Path, PathBuf};

mod png;

#[cfg(test)]
use png::png_crc32;
use png::validate_png;
pub use png::validate_png_dimensions;

/// Background selection for a direct image-generation request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImageBackground {
    /// Let the image provider choose the background treatment.
    Auto,
    /// Request an image with an alpha-capable transparent background.
    Transparent,
    /// Request an opaque image background.
    Opaque,
}

impl ImageBackground {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Transparent => "transparent",
            Self::Opaque => "opaque",
        }
    }
}

/// Quality selection for a direct image-generation request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImageQuality {
    /// Let the image provider choose the quality.
    Auto,
    /// Request low-cost generation.
    Low,
    /// Request medium-quality generation.
    Medium,
    /// Request high-quality generation.
    High,
}

impl ImageQuality {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// Size selection for a direct image-generation request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImageSize {
    /// Let the image provider choose the dimensions.
    Auto,
    /// Request a square 1024×1024 image.
    Square,
    /// Request a 1536×1024 landscape image.
    Landscape,
    /// Request a 1024×1536 portrait image.
    Portrait,
}

impl ImageSize {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Square => "1024x1024",
            Self::Landscape => "1536x1024",
            Self::Portrait => "1024x1536",
        }
    }
}

/// One direct, non-idempotent image-generation request.
///
/// The model is pinned internally to the official 0.150.1 image contract;
/// callers can select only the supported generation controls below.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectImageRequest {
    /// Natural-language image prompt.
    pub prompt: String,
    /// Requested background treatment.
    pub background: ImageBackground,
    /// Requested output dimensions.
    pub size: ImageSize,
    /// Requested generation quality.
    pub quality: ImageQuality,
}

/// One validated PNG returned by the official Images endpoint adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectImageResponse {
    pub(crate) base64: String,
    pub(crate) media_type: String,
    pub(crate) byte_length: usize,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) transparent_background: Option<bool>,
    pub(crate) alpha_capable: bool,
}

impl DirectImageResponse {
    /// Returns the base64-encoded PNG bytes.
    #[must_use]
    pub fn base64(&self) -> &str {
        &self.base64
    }

    /// Returns the MIME type reported by the official Images endpoint.
    #[must_use]
    pub fn media_type(&self) -> &str {
        &self.media_type
    }

    /// Returns the decoded PNG byte length.
    #[must_use]
    pub const fn byte_length(&self) -> usize {
        self.byte_length
    }

    /// Returns the validated PNG width.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Returns the validated PNG height.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Returns the provider's transparent-background metadata, when present.
    #[must_use]
    pub const fn transparent_background(&self) -> Option<bool> {
        self.transparent_background
    }

    /// Returns whether the PNG color format can carry an alpha channel.
    #[must_use]
    pub const fn alpha_capable(&self) -> bool {
        self.alpha_capable
    }
}

/// ChatGPT image endpoint used by the official Codex runtime.
pub(crate) const CHATGPT_IMAGE_GENERATION_ENDPOINT: &str =
    "https://chatgpt.com/backend-api/codex/images/generations";
const CODEX_VERSION: &str = "0.150.1";
// The official rust-v0.150.1 image extension pins this direct Images API model.
// It is intentionally not part of the caller-facing request contract.
const IMAGE_MODEL: &str = "gpt-image-2";
const ORIGINATOR: &str = "codex_cli_rs";
const MAX_IMAGE_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_IMAGE_JSON_BYTES: usize = 16 * 1024 * 1024;
const MAX_JWT_BYTES: usize = 256 * 1024;
const MAX_ACCOUNT_ID_BYTES: usize = 256;
static IMAGE_TURN_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub(crate) struct ChatGptImageAuth {
    access_token: String,
    account_id: String,
}

impl std::fmt::Debug for ChatGptImageAuth {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChatGptImageAuth")
            .field("access_token", &"[REDACTED]")
            .field("account_id", &"[REDACTED]")
            .finish()
    }
}

impl ChatGptImageAuth {
    pub(crate) fn from_auth_status(response: &Value) -> Result<Self, crate::error::Error> {
        let parsed: AuthStatus = serde_json::from_value(response.clone()).map_err(|error| {
            crate::error::Error::new(
                crate::error::ErrorKind::Authentication,
                "image.auth",
                format!("official app-server auth status was invalid: {error}"),
            )
        })?;
        if parsed.auth_method.as_deref() != Some("chatgpt") {
            return Err(crate::error::Error::new(
                crate::error::ErrorKind::Authentication,
                "image.auth",
                "official app-server did not report a ChatGPT login",
            ));
        }
        if parsed.requires_openai_auth == Some(false) {
            return Err(crate::error::Error::new(
                crate::error::ErrorKind::Authentication,
                "image.auth",
                "official app-server is configured without ChatGPT authentication",
            ));
        }
        let token = parsed.auth_token.ok_or_else(|| {
            crate::error::Error::new(
                crate::error::ErrorKind::Authentication,
                "image.auth",
                "official app-server did not export an access token",
            )
        })?;
        if token.is_empty() || token.len() > MAX_JWT_BYTES || token.contains('\0') {
            return Err(crate::error::Error::new(
                crate::error::ErrorKind::Authentication,
                "image.auth",
                "official app-server exported an invalid bounded access token",
            ));
        }
        let account_id = account_id_from_jwt(&token)?;
        Ok(Self {
            access_token: token,
            account_id,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthStatus {
    auth_method: Option<String>,
    auth_token: Option<String>,
    requires_openai_auth: Option<bool>,
}

fn account_id_from_jwt(token: &str) -> Result<String, crate::error::Error> {
    let mut segments = token.split('.');
    let _header = segments.next();
    let payload = segments.next().ok_or_else(|| {
        crate::error::Error::new(
            crate::error::ErrorKind::Authentication,
            "image.auth",
            "official app-server access token is not a JWT",
        )
    })?;
    if segments.next().is_none() || segments.next().is_some() || payload.is_empty() {
        return Err(crate::error::Error::new(
            crate::error::ErrorKind::Authentication,
            "image.auth",
            "official app-server access token has an invalid JWT shape",
        ));
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| URL_SAFE.decode(payload))
        .map_err(|_| {
            crate::error::Error::new(
                crate::error::ErrorKind::Authentication,
                "image.auth",
                "official app-server access token payload is not valid base64url",
            )
        })?;
    let claims: Value = serde_json::from_slice(&decoded).map_err(|_| {
        crate::error::Error::new(
            crate::error::ErrorKind::Authentication,
            "image.auth",
            "official app-server access token payload is not valid JSON",
        )
    })?;
    let account = claims
        .get("https://api.openai.com/auth")
        .and_then(Value::as_object)
        .and_then(|auth| auth.get("chatgpt_account_id"))
        .and_then(Value::as_str)
        .filter(|value| {
            !value.is_empty()
                && value.len() <= MAX_ACCOUNT_ID_BYTES
                && value.bytes().all(|byte| byte.is_ascii_graphic())
        })
        .ok_or_else(|| {
            crate::error::Error::new(
                crate::error::ErrorKind::Authentication,
                "image.auth",
                "official app-server access token lacks a bounded ChatGPT account claim",
            )
        })?;
    Ok(account.to_owned())
}

#[derive(Debug, Deserialize)]
struct ImageEndpointResponse {
    #[serde(default)]
    data: Vec<ImageEndpointData>,
}

#[derive(Debug, Deserialize)]
struct ImageEndpointData {
    b64_json: String,
}

#[derive(Debug)]
pub(crate) enum ImageEndpointError {
    Unauthorized,
    Failed(crate::error::Error),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ImageOperationPhase {
    Pending,
    Dispatched,
    CancelledBeforeDispatch,
    CancelledAfterDispatch,
    Finished,
}

/// Shared ownership state for one billed, non-idempotent image operation.
/// The worker owns only this state and its result channel; it never mutates
/// `ClientInner` directly.
#[derive(Debug)]
pub(crate) struct ImageOperationState {
    phase: Mutex<ImageOperationPhase>,
}

impl ImageOperationState {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            phase: Mutex::new(ImageOperationPhase::Pending),
        })
    }

    fn begin_dispatch(&self) -> Result<(), crate::error::Error> {
        let mut phase = self
            .phase
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match *phase {
            ImageOperationPhase::Pending => {
                *phase = ImageOperationPhase::Dispatched;
                Ok(())
            }
            ImageOperationPhase::CancelledBeforeDispatch
            | ImageOperationPhase::CancelledAfterDispatch => Err(crate::error::Error::new(
                crate::error::ErrorKind::Cancelled,
                "image.generate",
                "cancelled image operation cannot be dispatched again",
            )),
            ImageOperationPhase::Dispatched | ImageOperationPhase::Finished => {
                Err(crate::error::Error::new(
                    crate::error::ErrorKind::Protocol,
                    "image.generate",
                    "image operation was dispatched more than once",
                ))
            }
        }
    }

    fn reset_after_unauthorized(&self) -> bool {
        let mut phase = self
            .phase
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(*phase, ImageOperationPhase::Dispatched) {
            *phase = ImageOperationPhase::Pending;
            true
        } else {
            false
        }
    }

    fn finish(&self) {
        let mut phase = self
            .phase
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(*phase, ImageOperationPhase::Dispatched) {
            *phase = ImageOperationPhase::Finished;
        }
    }

    fn is_dispatched(&self) -> bool {
        matches!(
            *self
                .phase
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            ImageOperationPhase::Dispatched
                | ImageOperationPhase::CancelledAfterDispatch
                | ImageOperationPhase::Finished
        )
    }

    /// Marks the operation abandoned. Returns whether a request may already
    /// have reached the billed endpoint and therefore requires resolution.
    pub(crate) fn cancel(&self) -> bool {
        let mut phase = self
            .phase
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match *phase {
            ImageOperationPhase::Pending => {
                *phase = ImageOperationPhase::CancelledBeforeDispatch;
                false
            }
            ImageOperationPhase::Dispatched | ImageOperationPhase::Finished => {
                *phase = ImageOperationPhase::CancelledAfterDispatch;
                true
            }
            ImageOperationPhase::CancelledBeforeDispatch => false,
            ImageOperationPhase::CancelledAfterDispatch => true,
        }
    }

    pub(crate) fn cancellation_error(&self, turn_id: &str) -> crate::error::Error {
        let phase = *self
            .phase
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(
            phase,
            ImageOperationPhase::Dispatched
                | ImageOperationPhase::CancelledAfterDispatch
                | ImageOperationPhase::Finished
        ) {
            crate::error::Error::new(
                crate::error::ErrorKind::OutcomeUnknown,
                "image.generate",
                format!(
                    "image turn '{turn_id}' was dispatched but its result was abandoned; outcome is unknown and the request was not retried"
                ),
            )
        } else {
            crate::error::Error::new(
                crate::error::ErrorKind::Cancelled,
                "image.generate",
                format!("image turn '{turn_id}' was cancelled before dispatch"),
            )
        }
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        matches!(
            *self
                .phase
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            ImageOperationPhase::CancelledBeforeDispatch
                | ImageOperationPhase::CancelledAfterDispatch
        )
    }
}

pub(crate) async fn generate_via_endpoint(
    endpoint: String,
    auth: ChatGptImageAuth,
    request: DirectImageRequest,
    timeout: Duration,
    turn_id: String,
    operation: Arc<ImageOperationState>,
) -> Result<DirectImageResponse, ImageEndpointError> {
    tokio::task::spawn_blocking(move || {
        send_image_request(&endpoint, &auth, &request, timeout, &turn_id, &operation)
    })
    .await
    .map_err(|error| {
        ImageEndpointError::Failed(crate::error::Error::new(
            crate::error::ErrorKind::Process,
            "image.http",
            format!("image request worker failed: {error}"),
        ))
    })?
}

fn outcome_unknown_after_dispatch(
    turn_id: &str,
    cause: crate::error::Error,
) -> crate::error::Error {
    crate::error::Error::new(
        crate::error::ErrorKind::OutcomeUnknown,
        "image.generate",
        format!(
            "image turn '{turn_id}' was dispatched but its response could not be validated; outcome is unknown: {cause}"
        ),
    )
}

fn send_image_request(
    endpoint: &str,
    auth: &ChatGptImageAuth,
    request: &DirectImageRequest,
    timeout: Duration,
    turn_id: &str,
    operation: &ImageOperationState,
) -> Result<DirectImageResponse, ImageEndpointError> {
    let body = serde_json::json!({
        "prompt": request.prompt,
        "background": request.background.as_str(),
        "model": IMAGE_MODEL,
        "n": 1,
        "quality": request.quality.as_str(),
        "size": request.size.as_str(),
    });
    let body_bytes = serde_json::to_vec(&body).map_err(|error| {
        ImageEndpointError::Failed(crate::error::Error::new(
            crate::error::ErrorKind::Protocol,
            "image.http",
            format!("image request could not be encoded: {error}"),
        ))
    })?;
    if body_bytes.len() > 256 * 1024 {
        return Err(ImageEndpointError::Failed(crate::error::Error::new(
            crate::error::ErrorKind::ResourceLimit,
            "image.http",
            "image request body exceeds the bounded limit",
        )));
    }
    operation
        .begin_dispatch()
        .map_err(ImageEndpointError::Failed)?;
    let agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .timeout_global(Some(timeout))
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_body(Some(timeout))
        .build()
        .new_agent();
    let response = agent
        .post(endpoint)
        .header("Authorization", &format!("Bearer {}", auth.access_token))
        .header("Chatgpt-Account-Id", &auth.account_id)
        .header("Version", CODEX_VERSION)
        .header("Originator", ORIGINATOR)
        .header("x-codex-image-turn-id", turn_id)
        .content_type("application/json")
        .send(&body_bytes)
        .map_err(|error| {
            let kind = if operation.is_dispatched() {
                crate::error::ErrorKind::OutcomeUnknown
            } else {
                crate::error::ErrorKind::Disconnected
            };
            ImageEndpointError::Failed(crate::error::Error::new(
                kind,
                "image.http",
                format!("official image endpoint request failed: {error}"),
            ))
        })?;
    if response.status() == 401 {
        operation.reset_after_unauthorized();
        return Err(ImageEndpointError::Unauthorized);
    }
    if response.status() != 200 {
        operation.finish();
        return Err(ImageEndpointError::Failed(crate::error::Error::new(
            crate::error::ErrorKind::Rpc,
            "image.http",
            format!(
                "official image endpoint returned HTTP {}",
                response.status()
            ),
        )));
    }
    let bytes = response
        .into_body()
        .with_config()
        .limit(MAX_IMAGE_JSON_BYTES as u64)
        .read_to_vec()
        .map_err(|error| {
            ImageEndpointError::Failed(outcome_unknown_after_dispatch(
                turn_id,
                crate::error::Error::new(
                    crate::error::ErrorKind::Protocol,
                    "image.http",
                    format!("official image endpoint response could not be read: {error}"),
                ),
            ))
        })?;
    if operation.is_cancelled() {
        return Err(ImageEndpointError::Failed(
            operation.cancellation_error(turn_id),
        ));
    }
    match parse_image_endpoint_response(&bytes, request.background) {
        Ok(parsed) => {
            if operation.is_cancelled() {
                return Err(ImageEndpointError::Failed(
                    operation.cancellation_error(turn_id),
                ));
            }
            operation.finish();
            Ok(parsed)
        }
        Err(error) => Err(ImageEndpointError::Failed(outcome_unknown_after_dispatch(
            turn_id, error,
        ))),
    }
}

pub(crate) fn image_turn_id() -> String {
    let sequence = IMAGE_TURN_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "vergerail-image-{}-{nanos:016x}-{sequence}",
        std::process::id()
    )
}

fn parse_image_endpoint_response(
    bytes: &[u8],
    requested_background: ImageBackground,
) -> Result<DirectImageResponse, crate::error::Error> {
    let response: ImageEndpointResponse = serde_json::from_slice(bytes).map_err(|error| {
        crate::error::Error::new(
            crate::error::ErrorKind::Protocol,
            "image.http",
            format!("official image endpoint returned invalid JSON: {error}"),
        )
    })?;
    if response.data.len() != 1 {
        return Err(crate::error::Error::new(
            crate::error::ErrorKind::Protocol,
            "image.http",
            format!(
                "official image endpoint returned {} images; exactly one is required",
                response.data.len()
            ),
        ));
    }
    let encoded = &response.data[0].b64_json;
    if encoded.is_empty() || encoded.len() > MAX_IMAGE_RESPONSE_BYTES.div_ceil(3) * 4 {
        return Err(crate::error::Error::new(
            crate::error::ErrorKind::ResourceLimit,
            "image.http",
            "official image endpoint returned an oversized or empty image",
        ));
    }
    let decoded = BASE64_STANDARD.decode(encoded).map_err(|error| {
        crate::error::Error::new(
            crate::error::ErrorKind::Protocol,
            "image.http",
            format!("official image endpoint returned invalid PNG base64: {error}"),
        )
    })?;
    if decoded.len() > MAX_IMAGE_RESPONSE_BYTES {
        return Err(crate::error::Error::new(
            crate::error::ErrorKind::ResourceLimit,
            "image.http",
            "official image endpoint returned an oversized PNG",
        ));
    }
    let info = validate_png(&decoded)?;
    Ok(DirectImageResponse {
        base64: BASE64_STANDARD.encode(&decoded),
        media_type: "image/png".to_owned(),
        byte_length: decoded.len(),
        width: info.width,
        height: info.height,
        transparent_background: match requested_background {
            ImageBackground::Transparent => Some(info.has_transparent_pixels),
            ImageBackground::Auto | ImageBackground::Opaque => None,
        },
        alpha_capable: info.alpha_capable,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::ZlibEncoder;
    use serde_json::json;
    use std::io::{Read as _, Write as _};
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    fn auth_status(account_id: &str) -> Value {
        let payload = BASE64_STANDARD.encode(
            serde_json::to_vec(&json!({
                "https://api.openai.com/auth": {
                    "chatgpt_account_id": account_id
                }
            }))
            .expect("JWT payload"),
        );
        let encoded_payload =
            URL_SAFE_NO_PAD.encode(BASE64_STANDARD.decode(payload).expect("payload bytes"));
        json!({
            "authMethod": "chatgpt",
            "authToken": format!("e30.{encoded_payload}.sig"),
            "requiresOpenaiAuth": true
        })
    }

    fn auth(account_id: &str) -> ChatGptImageAuth {
        ChatGptImageAuth::from_auth_status(&auth_status(account_id)).expect("auth status")
    }

    #[test]
    fn image_auth_debug_never_exposes_credentials() {
        let auth = auth("private-account-id");
        for output in [format!("{auth:?}"), format!("{auth:#?}")] {
            assert!(!output.contains(&auth.access_token));
            assert!(!output.contains(&auth.account_id));
            assert!(output.contains("[REDACTED]"));
        }
    }

    fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut output = Vec::with_capacity(data.len() + 12);
        output.extend_from_slice(&(data.len() as u32).to_be_bytes());
        output.extend_from_slice(kind);
        output.extend_from_slice(data);
        output.extend_from_slice(&png_crc32(&output[4..]).to_be_bytes());
        output
    }

    fn png(color_type: u8, row: &[u8]) -> Vec<u8> {
        png_with_interlace(color_type, row, 0)
    }

    fn png_with_interlace(color_type: u8, row: &[u8], interlace: u8) -> Vec<u8> {
        let mut compressed = Vec::new();
        let mut encoder = ZlibEncoder::new(&mut compressed, Compression::default());
        encoder.write_all(row).expect("compress row");
        encoder.finish().expect("finish compressed row");
        let mut output = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&1u32.to_be_bytes());
        ihdr.extend_from_slice(&1u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, color_type, 0, 0, interlace]);
        output.extend_from_slice(&chunk(b"IHDR", &ihdr));
        output.extend_from_slice(&chunk(b"IDAT", &compressed));
        output.extend_from_slice(&chunk(b"IEND", &[]));
        output
    }

    fn png_with_dimensions(color_type: u8, width: u32, height: u32) -> Vec<u8> {
        let channels = match color_type {
            0 | 3 => 1,
            2 => 3,
            4 => 2,
            6 => 4,
            _ => panic!("unsupported test color type"),
        };
        let row = vec![0u8; width as usize * channels + 1];
        let mut compressed = Vec::new();
        let mut encoder = ZlibEncoder::new(&mut compressed, Compression::default());
        for _ in 0..height {
            encoder.write_all(&row).expect("compress row");
        }
        encoder.finish().expect("finish compressed rows");
        let mut output = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, color_type, 0, 0, 0]);
        output.extend_from_slice(&chunk(b"IHDR", &ihdr));
        output.extend_from_slice(&chunk(b"IDAT", &compressed));
        output.extend_from_slice(&chunk(b"IEND", &[]));
        output
    }

    fn png_with_chunks(
        color_type: u8,
        row: &[u8],
        before_idat: &[(&[u8; 4], &[u8])],
        after_idat: &[(&[u8; 4], &[u8])],
    ) -> Vec<u8> {
        let mut compressed = Vec::new();
        let mut encoder = ZlibEncoder::new(&mut compressed, Compression::default());
        encoder.write_all(row).expect("compress row");
        encoder.finish().expect("finish compressed row");
        let mut output = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&1u32.to_be_bytes());
        ihdr.extend_from_slice(&1u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, color_type, 0, 0, 0]);
        output.extend_from_slice(&chunk(b"IHDR", &ihdr));
        for (kind, data) in before_idat {
            output.extend_from_slice(&chunk(kind, data));
        }
        output.extend_from_slice(&chunk(b"IDAT", &compressed));
        for (kind, data) in after_idat {
            output.extend_from_slice(&chunk(kind, data));
        }
        output.extend_from_slice(&chunk(b"IEND", &[]));
        output
    }

    fn read_http_request(mut stream: &TcpStream) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 4096];
        let header_end = loop {
            let count = stream.read(&mut buffer).expect("request read");
            assert!(count > 0, "request must contain headers");
            bytes.extend_from_slice(&buffer[..count]);
            if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                break index + 4;
            }
            assert!(
                bytes.len() < 512 * 1024,
                "request headers must remain bounded"
            );
        };
        let headers = String::from_utf8_lossy(&bytes[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.strip_prefix("Content-Length:")
                    .or_else(|| line.strip_prefix("content-length:"))
            })
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        while bytes.len() < header_end + content_length {
            let count = stream.read(&mut buffer).expect("request body read");
            assert!(count > 0, "request body must complete");
            bytes.extend_from_slice(&buffer[..count]);
        }
        bytes
    }

    fn request_header(request: &[u8], name: &str) -> Option<String> {
        String::from_utf8_lossy(request).lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name)
                .then(|| value.trim().to_owned())
        })
    }

    fn serve_once(
        listener: TcpListener,
        status: u16,
        body: Vec<u8>,
    ) -> thread::JoinHandle<Vec<u8>> {
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("request connection");
            let request = read_http_request(&stream);
            let response = format!(
                "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .expect("response headers");
            stream.write_all(&body).expect("response body");
            request
        })
    }

    fn request() -> DirectImageRequest {
        DirectImageRequest {
            prompt: "a red fox isolated on transparent background".to_owned(),
            background: ImageBackground::Transparent,
            size: ImageSize::Square,
            quality: ImageQuality::High,
        }
    }

    #[test]
    fn auth_status_requires_chatgpt_token_and_bounded_account_claim() {
        let parsed = ChatGptImageAuth::from_auth_status(&auth_status("account-test"))
            .expect("ChatGPT auth status");
        assert_eq!(parsed.account_id, "account-test");
        assert!(
            ChatGptImageAuth::from_auth_status(&json!({
                "authMethod": "apiKey",
                "authToken": "not-a-jwt"
            }))
            .is_err()
        );
    }

    #[test]
    fn endpoint_request_contains_auth_headers_and_explicit_image_controls() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("local listener");
        let address = listener.local_addr().expect("listener address");
        let image = png(6, &[0, 255, 0, 0, 0]);
        let response = serde_json::to_vec(&json!({
            "created": 1,
            "data": [{"b64_json": BASE64_STANDARD.encode(&image)}]
        }))
        .expect("response JSON");
        let server = serve_once(listener, 200, response);
        let turn_id = image_turn_id();
        let operation = ImageOperationState::new();
        let result = send_image_request(
            &format!("http://{address}/images/generations"),
            &auth("account-test"),
            &request(),
            Duration::from_secs(5),
            &turn_id,
            &operation,
        )
        .expect("image response");
        let request_bytes = server.join().expect("server thread");
        let request_text = String::from_utf8_lossy(&request_bytes).to_ascii_lowercase();
        assert!(request_text.contains("authorization: bearer "));
        assert!(request_text.contains("chatgpt-account-id: account-test"));
        assert!(request_text.contains("version: 0.150.1"));
        assert!(request_text.contains("originator: codex_cli_rs"));
        assert!(request_text.contains("x-codex-image-turn-id: vergerail-image-"));
        let body = String::from_utf8_lossy(
            &request_bytes[request_bytes
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .expect("body separator")
                + 4..],
        );
        let body: Value = serde_json::from_str(&body).expect("request body JSON");
        assert_eq!(body["model"], "gpt-image-2");
        assert_eq!(body["n"], 1);
        assert_eq!(body["background"], "transparent");
        assert_eq!(body["size"], "1024x1024");
        assert_eq!(body["quality"], "high");
        assert!(result.alpha_capable());
        assert_eq!(result.transparent_background(), Some(true));
    }

    #[test]
    fn endpoint_401_supports_one_caller_refresh_retry() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("local listener");
        let address = listener.local_addr().expect("listener address");
        let image = png(6, &[0, 255, 0, 0, 0]);
        let body = serde_json::to_vec(&json!({
            "data": [{"b64_json": BASE64_STANDARD.encode(&image)}]
        }))
        .expect("response JSON");
        let server = thread::spawn(move || {
            let (mut first_stream, _) = listener.accept().expect("first connection");
            let first_request = read_http_request(&first_stream);
            first_stream
                .write_all(
                    b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .expect("401 response");
            let (mut second_stream, _) = listener.accept().expect("second connection");
            let second_request = read_http_request(&second_stream);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n",
                body.len()
            );
            second_stream
                .write_all(response.as_bytes())
                .expect("response headers");
            second_stream.write_all(&body).expect("response body");
            (first_request, second_request)
        });
        let endpoint = format!("http://{address}/images/generations");
        let turn_id = image_turn_id();
        let first_operation = ImageOperationState::new();
        let first_result = send_image_request(
            &endpoint,
            &auth("account-test"),
            &request(),
            Duration::from_secs(5),
            &turn_id,
            &first_operation,
        );
        assert!(matches!(
            first_result,
            Err(ImageEndpointError::Unauthorized)
        ));
        let second_result = send_image_request(
            &endpoint,
            &auth("refreshed-account"),
            &request(),
            Duration::from_secs(5),
            &turn_id,
            &first_operation,
        )
        .expect("one refresh retry response");
        assert!(second_result.alpha_capable());
        let (first_request, second_request) = server.join().expect("server");
        assert!(
            String::from_utf8_lossy(&first_request)
                .to_ascii_lowercase()
                .contains("chatgpt-account-id: account-test")
        );
        assert!(
            String::from_utf8_lossy(&second_request)
                .to_ascii_lowercase()
                .contains("chatgpt-account-id: refreshed-account")
        );
        assert_eq!(
            request_header(&first_request, "x-codex-image-turn-id"),
            request_header(&second_request, "x-codex-image-turn-id")
        );
    }

    #[test]
    fn endpoint_timeout_is_honored_instead_of_using_a_fixed_default() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("local listener");
        let address = listener.local_addr().expect("listener address");
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("request connection");
            let _request = read_http_request(&stream);
            thread::sleep(Duration::from_millis(250));
        });
        let started = std::time::Instant::now();
        let operation = ImageOperationState::new();
        let result = send_image_request(
            &format!("http://{address}/images/generations"),
            &auth("account-test"),
            &request(),
            Duration::from_millis(50),
            &image_turn_id(),
            &operation,
        );
        let elapsed = started.elapsed();
        assert!(
            matches!(result, Err(ImageEndpointError::Failed(error)) if error.kind() == crate::error::ErrorKind::OutcomeUnknown)
        );
        assert!(elapsed < Duration::from_millis(200), "elapsed={elapsed:?}");
        server.join().expect("server");
    }

    #[test]
    fn endpoint_other_status_is_not_retryable() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("local listener");
        let address = listener.local_addr().expect("listener address");
        let server = serve_once(listener, 429, Vec::new());
        let operation = ImageOperationState::new();
        let result = send_image_request(
            &format!("http://{address}/images/generations"),
            &auth("account-test"),
            &request(),
            Duration::from_secs(5),
            &image_turn_id(),
            &operation,
        );
        assert!(
            matches!(result, Err(ImageEndpointError::Failed(error)) if error.kind() == crate::error::ErrorKind::Rpc)
        );
        let request_bytes = server.join().expect("server");
        assert!(String::from_utf8_lossy(&request_bytes).contains("POST /images/generations"));
    }

    #[test]
    fn dispatched_http_200_validation_failures_are_unknown_and_not_replayed() {
        let image = png(6, &[0, 255, 0, 0, 0]);
        let mut invalid_crc = image.clone();
        let crc = invalid_crc.len() - 1;
        invalid_crc[crc] ^= 1;
        let responses = [
            b"not-json".to_vec(),
            serde_json::to_vec(&json!({
                "data": [{"b64_json": "not-base64"}]
            }))
            .expect("invalid data JSON"),
            serde_json::to_vec(&json!({
                "data": [{"b64_json": BASE64_STANDARD.encode(invalid_crc)}]
            }))
            .expect("invalid PNG JSON"),
        ];

        for (index, response) in responses.into_iter().enumerate() {
            let listener = TcpListener::bind("127.0.0.1:0").expect("local listener");
            let address = listener.local_addr().expect("listener address");
            let server = serve_once(listener, 200, response);
            let turn_id = format!("invalid-image-turn-{index}");
            let operation = ImageOperationState::new();
            let result = send_image_request(
                &format!("http://{address}/images/generations"),
                &auth("account-test"),
                &request(),
                Duration::from_secs(5),
                &turn_id,
                &operation,
            );
            assert!(matches!(
                result,
                Err(ImageEndpointError::Failed(error))
                    if error.kind() == crate::error::ErrorKind::OutcomeUnknown
                        && error.message().contains(&turn_id)
            ));
            assert!(matches!(
                operation.begin_dispatch(),
                Err(error) if error.kind() == crate::error::ErrorKind::Protocol
            ));
            server.join().expect("server thread");
        }
    }

    #[test]
    fn malformed_multiple_and_crc_invalid_png_results_are_rejected() {
        let image = png(6, &[0, 255, 0, 0, 0]);
        let one = || {
            serde_json::to_vec(&json!({"data": [{"b64_json": BASE64_STANDARD.encode(&image)}]}))
                .expect("JSON")
        };
        assert!(
            parse_image_endpoint_response(
                &serde_json::to_vec(&json!({"data": []})).expect("JSON"),
                ImageBackground::Auto
            )
            .is_err()
        );
        assert!(parse_image_endpoint_response(&serde_json::to_vec(&json!({"data": [{"b64_json": BASE64_STANDARD.encode(&image)}, {"b64_json": BASE64_STANDARD.encode(&image)}]})).expect("JSON"), ImageBackground::Auto).is_err());
        let mut invalid = image.clone();
        let crc = invalid.len() - 8;
        invalid[crc] ^= 1;
        assert!(parse_image_endpoint_response(&one(), ImageBackground::Auto).is_ok());
        assert!(
            parse_image_endpoint_response(
                &serde_json::to_vec(
                    &json!({"data": [{"b64_json": BASE64_STANDARD.encode(&invalid)}]})
                )
                .expect("JSON"),
                ImageBackground::Auto
            )
            .is_err()
        );
        let oversized = "A".repeat(MAX_IMAGE_RESPONSE_BYTES.div_ceil(3) * 4 + 1);
        assert!(
            parse_image_endpoint_response(
                &serde_json::to_vec(&json!({"data": [{"b64_json": oversized}]})).expect("JSON"),
                ImageBackground::Auto
            )
            .is_err()
        );

        let opaque = png(6, &[0, 255, 0, 0, 255]);
        let opaque_response = serde_json::to_vec(&json!({
            "data": [{"b64_json": BASE64_STANDARD.encode(&opaque)}]
        }))
        .expect("JSON");
        let parsed = parse_image_endpoint_response(&opaque_response, ImageBackground::Transparent)
            .expect("opaque RGBA PNG remains valid");
        assert!(parsed.alpha_capable());
        assert_eq!(parsed.transparent_background(), Some(false));
    }

    #[test]
    fn png_raw_limit_is_checked_before_decompression_and_exact_limit_is_valid() {
        let exact = png_with_dimensions(0, 8191, 1792);
        assert!(validate_png(&exact).is_ok());

        // RGBA dimensions below the 8192-pixel side limit can still encode
        // exactly one byte above the raw ceiling.
        let over = png_with_dimensions(6, 451, 8133);
        let error = validate_png(&over).expect_err("raw PNG limit");
        assert_eq!(error.kind(), crate::error::ErrorKind::ResourceLimit);
        assert!(error.message().contains("14680064"));
    }

    #[test]
    fn png_chunk_order_palette_transparency_and_critical_names_are_strict() {
        let palette = [0, 0, 0, 255, 0, 0];
        let valid_indexed = png_with_chunks(3, &[0, 1], &[(b"PLTE", &palette)], &[]);
        assert!(validate_png(&valid_indexed).is_ok());
        let indexed_transparent = png_with_chunks(
            3,
            &[0, 1],
            &[(b"PLTE", &palette), (b"tRNS", &[255, 0])],
            &[],
        );
        let indexed_info = validate_png(&indexed_transparent).expect("valid indexed tRNS");
        assert!(indexed_info.alpha_capable);
        assert!(indexed_info.has_transparent_pixels);

        let missing_palette = png(3, &[0, 0]);
        assert!(validate_png(&missing_palette).is_err());

        let late_palette = png_with_chunks(3, &[0, 0], &[], &[(b"PLTE", &palette)]);
        assert!(validate_png(&late_palette).is_err());

        let duplicate_palette =
            png_with_chunks(3, &[0, 0], &[(b"PLTE", &palette), (b"PLTE", &palette)], &[]);
        assert!(validate_png(&duplicate_palette).is_err());

        let invalid_trns = [255, 0, 0];
        let invalid_transparency =
            png_with_chunks(6, &[0, 255, 0, 0, 0], &[(b"tRNS", &invalid_trns)], &[]);
        assert!(validate_png(&invalid_transparency).is_err());

        let unknown_critical = png_with_chunks(6, &[0, 255, 0, 0, 0], &[(b"ABCD", &[])], &[]);
        assert!(validate_png(&unknown_critical).is_err());

        let noncontiguous_idat = png_with_chunks(
            6,
            &[0, 255, 0, 0, 0],
            &[],
            &[(b"tEXt", b"note"), (b"IDAT", &[])],
        );
        assert!(validate_png(&noncontiguous_idat).is_err());

        let adam7 = png_with_interlace(6, &[0, 255, 0, 0, 0], 1);
        assert!(validate_png(&adam7).is_err());
    }

    #[test]
    fn png_rgb_and_rgba_formats_remain_supported() {
        assert!(validate_png(&png(2, &[0, 255, 0, 0])).is_ok());
        assert!(validate_png(&png(6, &[0, 255, 0, 0, 255])).is_ok());
    }

    #[test]
    fn rgb_transparency_requires_three_valid_8_bit_samples() {
        let rgb_row = [0, 255, 0, 0];
        let key = [0, 255, 0, 0, 0, 0];
        let image = png_with_chunks(2, &rgb_row, &[(b"tRNS", &key)], &[]);
        let info = validate_png(&image).expect("valid RGB transparency");
        assert!(info.alpha_capable && info.has_transparent_pixels);

        for index in [0, 2, 4] {
            let mut invalid = key;
            invalid[index] = 1;
            let image = png_with_chunks(2, &rgb_row, &[(b"tRNS", &invalid)], &[]);
            let error = validate_png(&image).expect_err("nonzero high byte");
            assert!(error.message().contains("invalid transparency data"));
        }
        for invalid in [&key[..5], &[0, 255, 0, 0, 0, 0, 0][..]] {
            let image = png_with_chunks(2, &rgb_row, &[(b"tRNS", invalid)], &[]);
            let error = validate_png(&image).expect_err("invalid RGB key length");
            assert!(error.message().contains("invalid transparency data"));
        }
    }

    #[test]
    fn billed_image_operation_fences_pre_and_post_dispatch_cancellation() {
        let operation = ImageOperationState::new();
        assert!(!operation.cancel());
        assert_eq!(
            operation.cancellation_error("pre-dispatch-turn").kind(),
            crate::error::ErrorKind::Cancelled
        );
        assert!(matches!(
            operation.begin_dispatch(),
            Err(error) if error.kind() == crate::error::ErrorKind::Cancelled
        ));

        let operation = ImageOperationState::new();
        operation.begin_dispatch().expect("dispatch ownership");
        assert!(operation.cancel());
        assert_eq!(
            operation.cancellation_error("turn-1").kind(),
            crate::error::ErrorKind::OutcomeUnknown
        );
        operation.finish();
        assert_eq!(
            operation.cancellation_error("finished-turn").kind(),
            crate::error::ErrorKind::OutcomeUnknown
        );
        assert!(matches!(
            operation.begin_dispatch(),
            Err(error) if error.kind() == crate::error::ErrorKind::Cancelled
        ));
    }

    #[tokio::test]
    async fn billed_image_worker_reports_unknown_after_server_receives_request() {
        use std::io::ErrorKind as IoErrorKind;
        use std::sync::atomic::{AtomicBool, AtomicUsize};

        let listener = TcpListener::bind("127.0.0.1:0").expect("local listener");
        let address = listener.local_addr().expect("listener address");
        let (received_tx, received_rx) = std::sync::mpsc::channel();
        let release = Arc::new(AtomicBool::new(false));
        let accepted = Arc::new(AtomicUsize::new(0));
        let release_for_server = Arc::clone(&release);
        let accepted_for_server = Arc::clone(&accepted);
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("request connection");
            accepted_for_server.fetch_add(1, Ordering::Release);
            let _request = read_http_request(&stream);
            received_tx.send(()).expect("received signal");
            while !release_for_server.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(1));
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .expect("response");

            listener
                .set_nonblocking(true)
                .expect("nonblocking listener");
            let deadline = std::time::Instant::now() + Duration::from_millis(100);
            while std::time::Instant::now() < deadline {
                match listener.accept() {
                    Ok((extra, _)) => {
                        accepted_for_server.fetch_add(1, Ordering::Release);
                        let _ = extra.shutdown(std::net::Shutdown::Both);
                    }
                    Err(error) if error.kind() == IoErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("unexpected second accept error: {error}"),
                }
            }
        });

        let operation = ImageOperationState::new();
        let worker_operation = Arc::clone(&operation);
        let endpoint = format!("http://{address}/images/generations");
        let turn_id = "delayed-image-turn".to_owned();
        let worker = tokio::task::spawn_blocking(move || {
            send_image_request(
                &endpoint,
                &auth("account-test"),
                &request(),
                Duration::from_secs(5),
                &turn_id,
                &worker_operation,
            )
        });
        received_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("server observed request");
        assert!(
            operation.cancel(),
            "dispatch must be owned before cancellation"
        );
        release.store(true, Ordering::Release);

        let result = tokio::time::timeout(Duration::from_secs(1), worker)
            .await
            .expect("image worker must finish after cancellation")
            .expect("image worker task");
        assert!(matches!(
            result,
            Err(ImageEndpointError::Failed(error))
                if error.kind() == crate::error::ErrorKind::OutcomeUnknown
                    && error.message().contains("delayed-image-turn")
        ));
        let second = send_image_request(
            &format!("http://{address}/images/generations"),
            &auth("account-test"),
            &request(),
            Duration::from_secs(1),
            "delayed-image-turn",
            &operation,
        );
        assert!(matches!(
            second,
            Err(ImageEndpointError::Failed(error))
                if error.kind() == crate::error::ErrorKind::Cancelled
        ));
        server.join().expect("server thread");
        assert_eq!(accepted.load(Ordering::Acquire), 1);
    }
}

/// Provider-reported failure for an image-generation item.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ImageGenerationFailure {
    /// The account reached an image-generation usage limit.
    UsageLimitExceeded {
        /// Provider limit identifier.
        limit_id: String,
        /// Unix timestamp at which the limit resets, when reported.
        resets_at: Option<i64>,
    },
}

impl ImageGenerationFailure {
    fn retained_bytes(&self) -> usize {
        match self {
            Self::UsageLimitExceeded { limit_id, .. } => limit_id.len(),
        }
    }
}

/// One image-generation item from Codex app-server.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImageGeneration {
    id: String,
    status: String,
    revised_prompt: Option<String>,
    result_base64: String,
    transparent_background: Option<bool>,
    failure: Option<ImageGenerationFailure>,
    saved_path: Option<PathBuf>,
}

impl ImageGeneration {
    pub(crate) fn new(
        id: String,
        status: String,
        revised_prompt: Option<String>,
        result_base64: String,
        transparent_background: Option<bool>,
        failure: Option<ImageGenerationFailure>,
        saved_path: Option<PathBuf>,
    ) -> Self {
        Self {
            id,
            status,
            revised_prompt,
            result_base64,
            transparent_background,
            failure,
            saved_path,
        }
    }

    /// Returns the Codex item identifier.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the provider-defined lifecycle status.
    #[must_use]
    pub fn status(&self) -> &str {
        &self.status
    }

    /// Returns the revised prompt produced by the image model, when reported.
    #[must_use]
    pub fn revised_prompt(&self) -> Option<&str> {
        self.revised_prompt.as_deref()
    }

    /// Returns the base64-encoded generated image bytes.
    ///
    /// The value is empty while generation is in progress or when generation failed.
    #[must_use]
    pub fn result_base64(&self) -> &str {
        &self.result_base64
    }

    /// Returns whether a transparent background was requested or selected, when reported.
    #[must_use]
    pub const fn transparent_background(&self) -> Option<bool> {
        self.transparent_background
    }

    /// Returns the typed provider failure, when generation failed.
    #[must_use]
    pub const fn failure(&self) -> Option<&ImageGenerationFailure> {
        self.failure.as_ref()
    }

    /// Returns the sandboxed saved path, when Codex persisted the generated image.
    #[must_use]
    pub fn saved_path(&self) -> Option<&Path> {
        self.saved_path.as_deref()
    }

    pub(crate) fn retained_bytes(&self) -> Option<usize> {
        let mut bytes = self.id.len().checked_add(self.status.len())?;
        bytes = bytes.checked_add(self.result_base64.len())?;
        if let Some(prompt) = self.revised_prompt.as_deref() {
            bytes = bytes.checked_add(prompt.len())?;
        }
        if let Some(path) = self.saved_path.as_deref() {
            bytes = bytes.checked_add(path.as_os_str().len())?;
        }
        if let Some(failure) = self.failure.as_ref() {
            bytes = bytes.checked_add(failure.retained_bytes())?;
        }
        Some(bytes)
    }
}
