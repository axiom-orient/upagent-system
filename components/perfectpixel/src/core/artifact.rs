use serde::{de::Error as _, Deserialize, Deserializer, Serialize, Serializer};

use super::{PpError, PpResult};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Sha256Digest(String);

impl Sha256Digest {
    pub fn parse(value: impl Into<String>) -> PpResult<Self> {
        let value = value.into();
        if !super::sha256::is_sha256_hex(&value) {
            return Err(PpError::InvalidRequest(
                "SHA-256 digest must be 64 lowercase hexadecimal characters".to_string(),
            ));
        }
        Ok(Self(value))
    }

    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self(super::sha256::sha256_hex(bytes))
    }

    pub fn from_digest_bytes(digest: [u8; 32]) -> Self {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut value = String::with_capacity(64);
        for byte in digest {
            value.push(HEX[(byte >> 4) as usize] as char);
            value.push(HEX[(byte & 0x0f) as usize] as char);
        }
        Self(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for Sha256Digest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Sha256Digest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactRef {
    sha256: Sha256Digest,
    media_type: String,
    bytes: u64,
}

impl ArtifactRef {
    pub fn new(sha256: Sha256Digest, media_type: impl Into<String>, bytes: u64) -> PpResult<Self> {
        let media_type = media_type.into();
        validate_media_type(&media_type)?;
        Ok(Self {
            sha256,
            media_type,
            bytes,
        })
    }

    pub fn from_bytes(media_type: impl Into<String>, bytes: &[u8]) -> PpResult<Self> {
        let byte_count = u64::try_from(bytes.len())
            .map_err(|_| PpError::InvalidRequest("artifact byte count overflow".to_string()))?;
        Self::new(Sha256Digest::from_bytes(bytes), media_type, byte_count)
    }

    pub fn sha256(&self) -> &Sha256Digest {
        &self.sha256
    }

    pub fn media_type(&self) -> &str {
        &self.media_type
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ArtifactRefWire {
    sha256: Sha256Digest,
    media_type: String,
    bytes: u64,
}

impl<'de> Deserialize<'de> for ArtifactRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ArtifactRefWire::deserialize(deserializer)?;
        Self::new(wire.sha256, wire.media_type, wire.bytes).map_err(D::Error::custom)
    }
}

fn validate_media_type(value: &str) -> PpResult<()> {
    if value.is_empty()
        || value != value.trim()
        || value.len() > 255
        || value.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(PpError::InvalidRequest(
            "artifact media type must be a non-empty printable value up to 255 bytes".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_rejects_uppercase_hex() {
        assert!(Sha256Digest::parse("A".repeat(64)).is_err());
    }

    #[test]
    fn digest_bytes_encode_without_rehashing() {
        let digest = [0xabu8; 32];
        assert_eq!(
            Sha256Digest::from_digest_bytes(digest).as_str(),
            "ab".repeat(32)
        );
    }

    #[test]
    fn artifact_ref_from_bytes_uses_content_identity() -> PpResult<()> {
        let artifact = ArtifactRef::from_bytes("image/png", b"same bytes")?;
        assert_eq!(artifact.sha256(), &Sha256Digest::from_bytes(b"same bytes"));
        assert_eq!(artifact.bytes(), 10);
        Ok(())
    }
}
