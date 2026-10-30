use flate2::read::ZlibDecoder;
use std::io::Read as _;

use super::MAX_IMAGE_JSON_BYTES;

const MAX_PNG_RAW_BYTES: usize = 14 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PngInfo {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) alpha_capable: bool,
    pub(super) has_transparent_pixels: bool,
}

pub(super) fn validate_png(bytes: &[u8]) -> Result<PngInfo, crate::error::Error> {
    if bytes.len() < 33 || bytes.get(..8) != Some(b"\x89PNG\r\n\x1a\n") {
        return Err(crate::error::Error::new(
            crate::error::ErrorKind::Protocol,
            "image.http",
            "image response is not a PNG",
        ));
    }
    let mut offset = 8usize;
    let mut ihdr = None;
    let mut idat = Vec::new();
    let mut idat_seen = false;
    let mut idat_closed = false;
    let mut palette: Option<Vec<u8>> = None;
    let mut transparency: Option<Vec<u8>> = None;
    let mut saw_iend = false;
    while offset < bytes.len() {
        if bytes.len() - offset < 12 {
            return Err(crate::error::Error::new(
                crate::error::ErrorKind::Protocol,
                "image.http",
                "PNG chunk is truncated",
            ));
        }
        let length = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        let chunk_end = offset
            .checked_add(12)
            .and_then(|value| value.checked_add(length))
            .ok_or_else(|| {
                crate::error::Error::new(
                    crate::error::ErrorKind::ResourceLimit,
                    "image.http",
                    "PNG chunk length overflow",
                )
            })?;
        if chunk_end > bytes.len() {
            return Err(crate::error::Error::new(
                crate::error::ErrorKind::Protocol,
                "image.http",
                "PNG chunk exceeds response",
            ));
        }
        let chunk_type = &bytes[offset + 4..offset + 8];
        let data = &bytes[offset + 8..offset + 8 + length];
        let observed_crc =
            u32::from_be_bytes(bytes[offset + 8 + length..chunk_end].try_into().unwrap());
        if png_crc32(&bytes[offset + 4..offset + 8 + length]) != observed_crc {
            return Err(crate::error::Error::new(
                crate::error::ErrorKind::Protocol,
                "image.http",
                "PNG chunk CRC mismatch",
            ));
        }
        if saw_iend {
            return Err(crate::error::Error::new(
                crate::error::ErrorKind::Protocol,
                "image.http",
                "PNG contains a chunk after IEND",
            ));
        }
        match chunk_type {
            b"IHDR" => {
                if ihdr.is_some() || length != 13 || offset != 8 {
                    return Err(crate::error::Error::new(
                        crate::error::ErrorKind::Protocol,
                        "image.http",
                        "PNG has an invalid IHDR",
                    ));
                }
                let width = u32::from_be_bytes(data[0..4].try_into().unwrap());
                let height = u32::from_be_bytes(data[4..8].try_into().unwrap());
                let bit_depth = data[8];
                let color_type = data[9];
                if width == 0
                    || height == 0
                    || width > 8192
                    || height > 8192
                    || bit_depth != 8
                    || !matches!(color_type, 0 | 2 | 3 | 4 | 6)
                    || data[10] != 0
                    || data[11] != 0
                    || data[12] != 0
                {
                    return Err(crate::error::Error::new(
                        crate::error::ErrorKind::Protocol,
                        "image.http",
                        "PNG IHDR dimensions or format are unsupported",
                    ));
                }
                let expected_raw = png_expected_raw_bytes(width, height, color_type)?;
                if expected_raw > MAX_PNG_RAW_BYTES {
                    return Err(crate::error::Error::new(
                        crate::error::ErrorKind::ResourceLimit,
                        "image.http",
                        format!("PNG decompressed data exceeds {MAX_PNG_RAW_BYTES} bytes"),
                    ));
                }
                ihdr = Some((width, height, color_type));
            }
            b"PLTE" => {
                let Some((_, _, color_type)) = ihdr else {
                    return Err(crate::error::Error::new(
                        crate::error::ErrorKind::Protocol,
                        "image.http",
                        "PNG PLTE appears before IHDR",
                    ));
                };
                if idat_seen
                    || palette.is_some()
                    || length == 0
                    || length > 768
                    || !length.is_multiple_of(3)
                    || (color_type == 0 || color_type == 4)
                {
                    return Err(crate::error::Error::new(
                        crate::error::ErrorKind::Protocol,
                        "image.http",
                        "PNG has an invalid or misplaced PLTE",
                    ));
                }
                palette = Some(data.to_vec());
            }
            b"tRNS" => {
                let Some((_, _, color_type)) = ihdr else {
                    return Err(crate::error::Error::new(
                        crate::error::ErrorKind::Protocol,
                        "image.http",
                        "PNG tRNS appears before IHDR",
                    ));
                };
                if idat_seen || transparency.is_some() {
                    return Err(crate::error::Error::new(
                        crate::error::ErrorKind::Protocol,
                        "image.http",
                        "PNG has a duplicate or misplaced tRNS",
                    ));
                }
                let valid = match color_type {
                    0 => length == 2 && data[0] == 0,
                    2 => length == 6 && data.as_chunks::<2>().0.iter().all(|sample| sample[0] == 0),
                    3 => palette
                        .as_ref()
                        .is_some_and(|colors| length <= colors.len() / 3 && length > 0),
                    _ => false,
                };
                if !valid {
                    return Err(crate::error::Error::new(
                        crate::error::ErrorKind::Protocol,
                        "image.http",
                        "PNG has invalid transparency data",
                    ));
                }
                transparency = Some(data.to_vec());
            }
            b"IDAT" => {
                let Some((_, _, color_type)) = ihdr else {
                    return Err(crate::error::Error::new(
                        crate::error::ErrorKind::Protocol,
                        "image.http",
                        "PNG IDAT is missing IHDR",
                    ));
                };
                if (color_type == 3 && palette.is_none()) || idat_closed {
                    return Err(crate::error::Error::new(
                        crate::error::ErrorKind::Protocol,
                        "image.http",
                        if color_type == 3 && palette.is_none() {
                            "indexed PNG IDAT appears before PLTE"
                        } else {
                            "PNG IDAT is not contiguous"
                        },
                    ));
                }
                idat_seen = true;
                idat.extend_from_slice(data);
                if idat.len() > MAX_IMAGE_JSON_BYTES {
                    return Err(crate::error::Error::new(
                        crate::error::ErrorKind::ResourceLimit,
                        "image.http",
                        "PNG compressed data exceeds the bound",
                    ));
                }
            }
            b"IEND" => {
                if length != 0 || !idat_seen {
                    return Err(crate::error::Error::new(
                        crate::error::ErrorKind::Protocol,
                        "image.http",
                        "PNG has an invalid IEND",
                    ));
                }
                saw_iend = true;
                if chunk_end != bytes.len() {
                    return Err(crate::error::Error::new(
                        crate::error::ErrorKind::Protocol,
                        "image.http",
                        "PNG contains data after IEND",
                    ));
                }
            }
            _ => {
                if ihdr.is_none() {
                    return Err(crate::error::Error::new(
                        crate::error::ErrorKind::Protocol,
                        "image.http",
                        "PNG contains a chunk before IHDR",
                    ));
                }
                if chunk_type[0].is_ascii_uppercase() {
                    return Err(crate::error::Error::new(
                        crate::error::ErrorKind::Protocol,
                        "image.http",
                        "PNG contains an unknown critical chunk",
                    ));
                }
                if idat_seen {
                    idat_closed = true;
                }
            }
        }
        if !matches!(chunk_type, b"IDAT" | b"IEND") && idat_seen {
            idat_closed = true;
        }
        offset = chunk_end;
    }
    let (width, height, color_type) = ihdr.ok_or_else(|| {
        crate::error::Error::new(
            crate::error::ErrorKind::Protocol,
            "image.http",
            "PNG is missing IHDR",
        )
    })?;
    if !saw_iend {
        return Err(crate::error::Error::new(
            crate::error::ErrorKind::Protocol,
            "image.http",
            "PNG is missing IEND",
        ));
    }
    if color_type == 3 && palette.is_none() {
        return Err(crate::error::Error::new(
            crate::error::ErrorKind::Protocol,
            "image.http",
            "indexed PNG is missing PLTE",
        ));
    }
    let channels = png_channels(color_type);
    let decoder = ZlibDecoder::new(idat.as_slice());
    let mut decoder = decoder;
    let row_bytes = (width as usize).checked_mul(channels).ok_or_else(|| {
        crate::error::Error::new(
            crate::error::ErrorKind::ResourceLimit,
            "image.http",
            "PNG row size exceeds decoder bounds",
        )
    })?;
    let mut previous = vec![0u8; row_bytes];
    let mut current = vec![0u8; row_bytes];
    let mut has_transparent_pixels = false;
    let transparent_gray = transparency
        .as_deref()
        .filter(|_| color_type == 0)
        .map(|value| value[1]);
    let transparent_rgb = transparency
        .as_deref()
        .filter(|_| color_type == 2)
        .map(|value| [value[1], value[3], value[5]]);
    let palette = palette.as_deref();
    for _row in 0..height as usize {
        let mut filter = [0u8; 1];
        decoder.read_exact(&mut filter).map_err(|_| {
            crate::error::Error::new(
                crate::error::ErrorKind::Protocol,
                "image.http",
                "PNG IDAT is not valid zlib data",
            )
        })?;
        decoder.read_exact(&mut current).map_err(|_| {
            crate::error::Error::new(
                crate::error::ErrorKind::Protocol,
                "image.http",
                "PNG decompressed data length does not match IHDR",
            )
        })?;
        for index in 0..row_bytes {
            let value = current[index];
            let left = if index >= channels {
                current[index - channels]
            } else {
                0
            };
            let up = previous[index];
            let upper_left = if index >= channels {
                previous[index - channels]
            } else {
                0
            };
            current[index] = match filter[0] {
                0 => value,
                1 => value.wrapping_add(left),
                2 => value.wrapping_add(up),
                3 => value.wrapping_add(((u16::from(left) + u16::from(up)) / 2) as u8),
                4 => value.wrapping_add(paeth_predictor(left, up, upper_left)),
                _ => {
                    return Err(crate::error::Error::new(
                        crate::error::ErrorKind::Protocol,
                        "image.http",
                        "PNG row uses an unsupported filter",
                    ));
                }
            };
        }
        for pixel in 0..width as usize {
            let start = pixel * channels;
            has_transparent_pixels |= match color_type {
                0 => transparent_gray.is_some_and(|sample| current[start] == sample),
                2 => transparent_rgb.is_some_and(|sample| current[start..start + 3] == sample),
                3 => {
                    let index = current[start] as usize;
                    let Some(palette) = palette else {
                        return Err(crate::error::Error::new(
                            crate::error::ErrorKind::Protocol,
                            "image.http",
                            "indexed PNG is missing PLTE",
                        ));
                    };
                    if index >= palette.len() / 3 {
                        return Err(crate::error::Error::new(
                            crate::error::ErrorKind::Protocol,
                            "image.http",
                            "indexed PNG pixel is outside PLTE",
                        ));
                    }
                    transparency
                        .as_deref()
                        .is_some_and(|alpha| alpha.get(index).copied().unwrap_or(u8::MAX) < u8::MAX)
                }
                4 => current[start + 1] < u8::MAX,
                6 => current[start + 3] < u8::MAX,
                _ => false,
            };
        }
        std::mem::swap(&mut previous, &mut current);
    }
    let mut extra = [0u8; 1];
    match decoder.read(&mut extra) {
        Ok(0) => {}
        Ok(_) => {
            return Err(crate::error::Error::new(
                crate::error::ErrorKind::Protocol,
                "image.http",
                "PNG decompressed data contains trailing bytes",
            ));
        }
        Err(_) => {
            return Err(crate::error::Error::new(
                crate::error::ErrorKind::Protocol,
                "image.http",
                "PNG IDAT is not valid zlib data",
            ));
        }
    }
    Ok(PngInfo {
        width,
        height,
        alpha_capable: matches!(color_type, 4 | 6) || transparency.is_some(),
        has_transparent_pixels,
    })
}

/// Validates a bounded PNG payload and returns its declared raster dimensions.
///
/// This is shared by the official image adapter and the UpAgent provider so
/// both ingress paths enforce the same chunk, CRC, zlib, and decompressed
/// scanline invariants before accepting image bytes.
pub fn validate_png_dimensions(bytes: &[u8]) -> crate::Result<(u32, u32)> {
    let info = validate_png(bytes)?;
    Ok((info.width, info.height))
}

fn png_channels(color_type: u8) -> usize {
    match color_type {
        0 | 3 => 1,
        2 => 3,
        4 => 2,
        6 => 4,
        _ => unreachable!(),
    }
}

fn png_expected_raw_bytes(
    width: u32,
    height: u32,
    color_type: u8,
) -> Result<usize, crate::error::Error> {
    let channels = png_channels(color_type);
    (width as usize)
        .checked_mul(channels)
        .and_then(|row| row.checked_add(1))
        .and_then(|row| row.checked_mul(height as usize))
        .ok_or_else(|| {
            crate::error::Error::new(
                crate::error::ErrorKind::ResourceLimit,
                "image.http",
                "PNG dimensions exceed decoder bounds",
            )
        })
}

fn paeth_predictor(left: u8, up: u8, upper_left: u8) -> u8 {
    let left = i32::from(left);
    let up = i32::from(up);
    let upper_left = i32::from(upper_left);
    let estimate = left + up - upper_left;
    let left_distance = (estimate - left).abs();
    let up_distance = (estimate - up).abs();
    let upper_left_distance = (estimate - upper_left).abs();
    if left_distance <= up_distance && left_distance <= upper_left_distance {
        left as u8
    } else if up_distance <= upper_left_distance {
        up as u8
    } else {
        upper_left as u8
    }
}

pub(super) fn png_crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}
