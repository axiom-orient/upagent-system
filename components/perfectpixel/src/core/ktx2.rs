use serde::{Deserialize, Serialize};

use super::{PpError, PpResult};

const KTX2_IDENTIFIER: &[u8; 12] = b"\xABKTX 20\xBB\r\n\x1A\n";
const KTX2_HEADER_BYTES: usize = 80;
const KTX2_LEVEL_INDEX_BYTES: usize = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextureSemantic {
    ColorSrgb,
    Linear,
    NormalMap,
    Mask,
}

impl TextureSemantic {
    pub fn is_srgb(self) -> bool {
        matches!(self, Self::ColorSrgb)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Ktx2Info {
    pub vk_format: u32,
    pub type_size: u32,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub layer_count: u32,
    pub face_count: u32,
    pub level_count: u32,
    pub supercompression_scheme: u32,
    pub dfd_transfer_function: u8,
}

pub fn inspect_ktx2(bytes: &[u8]) -> PpResult<Ktx2Info> {
    if bytes.len() < KTX2_HEADER_BYTES || &bytes[..12] != KTX2_IDENTIFIER {
        return Err(PpError::InvalidRequest(
            "invalid KTX2 identifier/header".to_string(),
        ));
    }
    let vk_format = le32(bytes, 12)?;
    let type_size = le32(bytes, 16)?;
    let width = le32(bytes, 20)?;
    let height = le32(bytes, 24)?;
    let depth = le32(bytes, 28)?;
    let layer_count = le32(bytes, 32)?;
    let face_count = le32(bytes, 36)?;
    let level_count = le32(bytes, 40)?;
    let supercompression_scheme = le32(bytes, 44)?;
    let dfd_offset = le32(bytes, 48)? as usize;
    let dfd_length = le32(bytes, 52)? as usize;
    let kvd_offset = le32(bytes, 56)? as usize;
    let kvd_length = le32(bytes, 60)? as usize;
    let sgd_offset = le64(bytes, 64)? as usize;
    let sgd_length = le64(bytes, 72)? as usize;

    if width == 0 || height == 0 || depth != 0 || layer_count != 0 || face_count != 1 {
        return Err(PpError::InvalidRequest(
            "PerfectPixel texture.compile accepts only non-array 2D KTX2 textures".to_string(),
        ));
    }
    if type_size == 0 || level_count == 0 || level_count > 32 {
        return Err(PpError::InvalidRequest(
            "KTX2 typeSize/levelCount is outside bounded contract".to_string(),
        ));
    }
    let level_index_end = KTX2_HEADER_BYTES
        .checked_add(level_count as usize * KTX2_LEVEL_INDEX_BYTES)
        .ok_or_else(|| PpError::InvalidRequest("KTX2 level index overflow".to_string()))?;
    if level_index_end > bytes.len() {
        return Err(PpError::InvalidRequest(
            "KTX2 level index is truncated".to_string(),
        ));
    }
    check_section(bytes, dfd_offset, dfd_length, "DFD", true)?;
    check_section(bytes, kvd_offset, kvd_length, "KVD", false)?;
    check_section(bytes, sgd_offset, sgd_length, "SGD", false)?;
    if dfd_offset < level_index_end || dfd_length < 16 {
        return Err(PpError::InvalidRequest(
            "KTX2 DFD overlaps header/index or is too short".to_string(),
        ));
    }
    let dfd_total = le32(bytes, dfd_offset)? as usize;
    if dfd_total > dfd_length || dfd_total < 16 {
        return Err(PpError::InvalidRequest(
            "KTX2 DFD total size is invalid".to_string(),
        ));
    }
    let dfd_transfer_function = bytes[dfd_offset + 14];

    let mut previous_offset = 0usize;
    for index in 0..level_count as usize {
        let base = KTX2_HEADER_BYTES + index * KTX2_LEVEL_INDEX_BYTES;
        let offset = le64(bytes, base)? as usize;
        let length = le64(bytes, base + 8)? as usize;
        let uncompressed_length = le64(bytes, base + 16)? as usize;
        if length == 0 {
            return Err(PpError::InvalidRequest(format!(
                "KTX2 level {index} has zero byte length"
            )));
        }
        offset
            .checked_add(length)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| PpError::InvalidRequest(format!("KTX2 level {index} exceeds file")))?;
        if offset < level_index_end || (index > 0 && offset >= previous_offset) {
            // Level Index is base-level first while a mip array is stored small-level first,
            // therefore offsets strictly decrease as the index advances.
            return Err(PpError::InvalidRequest(
                "KTX2 level payload ordering/offset is invalid".to_string(),
            ));
        }
        validate_level_lengths(supercompression_scheme, length, uncompressed_length, index)?;
        previous_offset = offset;
    }

    Ok(Ktx2Info {
        vk_format,
        type_size,
        width,
        height,
        depth,
        layer_count,
        face_count,
        level_count,
        supercompression_scheme,
        dfd_transfer_function,
    })
}

pub fn verify_ktx2_contract(
    info: &Ktx2Info,
    width: u32,
    height: u32,
    semantic: TextureSemantic,
    generate_mipmaps: bool,
    basis_lz: bool,
) -> PpResult<()> {
    if info.width != width || info.height != height {
        return Err(PpError::QualityGate {
            gate: "ktx2_dimensions".to_string(),
            message: format!(
                "expected {width}x{height}, got {}x{}",
                info.width, info.height
            ),
        });
    }
    let expected_levels = if generate_mipmaps {
        mip_level_count(width, height)
    } else {
        1
    };
    if info.level_count != expected_levels {
        return Err(PpError::QualityGate {
            gate: "ktx2_mip_count".to_string(),
            message: format!("expected {expected_levels}, got {}", info.level_count),
        });
    }
    let expected_supercompression = if basis_lz { 1 } else { 0 };
    if info.supercompression_scheme != expected_supercompression {
        return Err(PpError::QualityGate {
            gate: "ktx2_supercompression".to_string(),
            message: format!(
                "expected scheme {expected_supercompression}, got {}",
                info.supercompression_scheme
            ),
        });
    }
    // KDF transfer-function values: 1 = linear, 2 = sRGB.
    let expected_transfer = if semantic.is_srgb() { 2 } else { 1 };
    if info.dfd_transfer_function != expected_transfer {
        return Err(PpError::QualityGate {
            gate: "ktx2_transfer_function".to_string(),
            message: format!(
                "semantic {semantic:?} requires transfer {expected_transfer}, got {}",
                info.dfd_transfer_function
            ),
        });
    }
    Ok(())
}

pub fn mip_level_count(width: u32, height: u32) -> u32 {
    let maximum = width.max(height);
    u32::BITS - maximum.leading_zeros()
}

fn validate_level_lengths(
    supercompression_scheme: u32,
    byte_length: usize,
    uncompressed_byte_length: usize,
    index: usize,
) -> PpResult<()> {
    match supercompression_scheme {
        // KTX 2.0.4 §3.9.7: without supercompression both values are identical.
        0 if uncompressed_byte_length != byte_length => Err(PpError::InvalidRequest(format!(
            "KTX2 level {index} uncompressed byte length must equal byte length without supercompression"
        ))),
        // KTX 2.0.4 §3.9.7 and §3.8: BasisLZ has no reflated-size value in the level index.
        1 if uncompressed_byte_length != 0 => Err(PpError::InvalidRequest(format!(
            "BasisLZ KTX2 level {index} uncompressed byte length must be zero"
        ))),
        // Zstd/Zlib and registered non-Basis schemes describe reflated byte length.
        scheme if scheme != 0 && scheme != 1 && uncompressed_byte_length == 0 => {
            Err(PpError::InvalidRequest(format!(
                "supercompressed KTX2 level {index} for scheme {scheme} lacks uncompressed byte length"
            )))
        }
        _ => Ok(()),
    }
}

fn check_section(
    bytes: &[u8],
    offset: usize,
    length: usize,
    label: &str,
    required: bool,
) -> PpResult<()> {
    if length == 0 {
        if required {
            return Err(PpError::InvalidRequest(format!("KTX2 {label} is required")));
        }
        if offset != 0 {
            return Err(PpError::InvalidRequest(format!(
                "KTX2 {label} offset must be zero when length is zero"
            )));
        }
        return Ok(());
    }
    if offset == 0
        || offset
            .checked_add(length)
            .is_none_or(|end| end > bytes.len())
    {
        return Err(PpError::InvalidRequest(format!(
            "KTX2 {label} section is outside the file"
        )));
    }
    Ok(())
}

fn le32(bytes: &[u8], offset: usize) -> PpResult<u32> {
    let end = offset
        .checked_add(4)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| PpError::InvalidRequest("truncated KTX2 u32".to_string()))?;
    Ok(u32::from_le_bytes(bytes[offset..end].try_into().unwrap()))
}

fn le64(bytes: &[u8], offset: usize) -> PpResult<u64> {
    let end = offset
        .checked_add(8)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| PpError::InvalidRequest("truncated KTX2 u64".to_string()))?;
    Ok(u64::from_le_bytes(bytes[offset..end].try_into().unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mip_count_is_exact() {
        assert_eq!(mip_level_count(1, 1), 1);
        assert_eq!(mip_level_count(8, 4), 4);
        assert_eq!(mip_level_count(9, 9), 4);
    }

    #[test]
    fn invalid_identifier_fails_closed() {
        assert!(inspect_ktx2(&[0u8; 80]).is_err());
    }

    #[test]
    fn level_lengths_follow_ktx_2_0_4_supercompression_contract() {
        assert!(validate_level_lengths(0, 128, 128, 0).is_ok());
        assert!(validate_level_lengths(0, 128, 0, 0).is_err());
        assert!(validate_level_lengths(1, 64, 0, 0).is_ok());
        assert!(validate_level_lengths(1, 64, 128, 0).is_err());
        assert!(validate_level_lengths(2, 64, 128, 0).is_ok());
        assert!(validate_level_lengths(3, 64, 0, 0).is_err());
    }
}
