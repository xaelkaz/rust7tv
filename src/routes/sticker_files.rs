//! Checks a WhatsApp sticker uploaded by the app before it is stored and served to others.
//!
//! Only the WebP container and frame headers are read; nothing is decoded. The app validates
//! the full animation again after downloading, and the admin reviews the files before approving.

/// WhatsApp's sticker canvas.
pub const CANVAS_PX: u32 = 512;
/// WhatsApp's size limits; they match the app's `WhatsAppStickerRequirements`.
pub const ANIMATED_MAX_BYTES: usize = 500_000;
pub const STATIC_MAX_BYTES: usize = 100 * 1024;

#[derive(Debug, PartialEq)]
pub struct WebpInfo {
    pub width: u32,
    pub height: u32,
    pub animated: bool,
}

/// Reads the size and kind of a WebP file, or says why it isn't one.
pub fn inspect_webp(bytes: &[u8]) -> Result<WebpInfo, &'static str> {
    if bytes.len() < 30 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return Err("Not a WebP file");
    }
    let riff_size = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
    if riff_size + 8 != bytes.len() {
        return Err("Truncated or padded WebP file");
    }

    let chunks = read_chunks(&bytes[12..])?;
    let (first_tag, first_data) = chunks.first().ok_or("Empty WebP file")?;
    match *first_tag {
        b"VP8X" => {
            if first_data.len() < 10 {
                return Err("Bad extended header");
            }
            let animated = first_data[0] & 0x02 != 0;
            let width = 1 + u24(&first_data[4..7]);
            let height = 1 + u24(&first_data[7..10]);
            let has = |tag: &[u8; 4]| chunks.iter().any(|(t, _)| *t == tag);
            if animated && (!has(b"ANIM") || !has(b"ANMF")) {
                return Err("Animated WebP without frames");
            }
            if !animated && !has(b"VP8 ") && !has(b"VP8L") {
                return Err("Static WebP without an image");
            }
            Ok(WebpInfo { width, height, animated })
        }
        b"VP8 " => {
            // Frame tag (3 bytes), start code 9d 01 2a, then 14-bit width and height.
            if first_data.len() < 10 || first_data[3..6] != [0x9d, 0x01, 0x2a] {
                return Err("Bad lossy header");
            }
            let width = u32::from(u16::from_le_bytes([first_data[6], first_data[7]]) & 0x3fff);
            let height = u32::from(u16::from_le_bytes([first_data[8], first_data[9]]) & 0x3fff);
            Ok(WebpInfo { width, height, animated: false })
        }
        b"VP8L" => {
            // Signature 0x2f, then 14 bits of width - 1 and 14 bits of height - 1.
            if first_data.len() < 5 || first_data[0] != 0x2f {
                return Err("Bad lossless header");
            }
            let bits = u32::from_le_bytes([first_data[1], first_data[2], first_data[3], first_data[4]]);
            Ok(WebpInfo {
                width: (bits & 0x3fff) + 1,
                height: ((bits >> 14) & 0x3fff) + 1,
                animated: false,
            })
        }
        _ => Err("Unknown WebP format"),
    }
}

/// Checks an uploaded file against WhatsApp's rules for a pack of the given kind.
pub fn validate_sticker(bytes: &[u8], pack_animated: bool) -> Result<(), String> {
    let max = if pack_animated { ANIMATED_MAX_BYTES } else { STATIC_MAX_BYTES };
    if bytes.len() > max {
        return Err(format!("Sticker is larger than {max} bytes"));
    }
    let info = inspect_webp(bytes).map_err(str::to_string)?;
    if info.width != CANVAS_PX || info.height != CANVAS_PX {
        return Err(format!("Sticker must be {CANVAS_PX}x{CANVAS_PX}, got {}x{}", info.width, info.height));
    }
    if info.animated != pack_animated {
        return Err(if pack_animated {
            "This pack takes animated stickers".to_string()
        } else {
            "This pack takes static stickers".to_string()
        });
    }
    Ok(())
}

fn u24(bytes: &[u8]) -> u32 {
    u32::from(bytes[0]) | u32::from(bytes[1]) << 8 | u32::from(bytes[2]) << 16
}

/// A RIFF chunk: its fourcc tag and its data.
type Chunk<'a> = (&'a [u8; 4], &'a [u8]);

/// Splits RIFF chunks (fourcc, little-endian size, data padded to an even length).
fn read_chunks(mut rest: &[u8]) -> Result<Vec<Chunk<'_>>, &'static str> {
    let mut chunks = Vec::new();
    while !rest.is_empty() {
        if rest.len() < 8 {
            return Err("Truncated chunk header");
        }
        let tag: &[u8; 4] = rest[0..4].try_into().map_err(|_| "Bad chunk tag")?;
        let size = u32::from_le_bytes([rest[4], rest[5], rest[6], rest[7]]) as usize;
        let padded = size + (size & 1);
        if rest.len() < 8 + size {
            return Err("Truncated chunk");
        }
        chunks.push((tag, &rest[8..8 + size]));
        rest = &rest[(8 + padded).min(rest.len())..];
    }
    Ok(chunks)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn riff(chunks: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut body = b"WEBP".to_vec();
        for (tag, data) in chunks {
            body.extend_from_slice(*tag);
            body.extend_from_slice(&(data.len() as u32).to_le_bytes());
            body.extend_from_slice(data);
            if data.len() % 2 == 1 {
                body.push(0);
            }
        }
        let mut file = b"RIFF".to_vec();
        file.extend_from_slice(&(body.len() as u32).to_le_bytes());
        file.extend(body);
        file
    }

    fn vp8x(animated: bool, width: u32, height: u32) -> Vec<u8> {
        let mut data = vec![if animated { 0x02 } else { 0x00 }, 0, 0, 0];
        data.extend_from_slice(&(width - 1).to_le_bytes()[..3]);
        data.extend_from_slice(&(height - 1).to_le_bytes()[..3]);
        data
    }

    fn vp8(width: u16, height: u16) -> Vec<u8> {
        let mut data = vec![0x10, 0x02, 0x00, 0x9d, 0x01, 0x2a];
        data.extend_from_slice(&width.to_le_bytes());
        data.extend_from_slice(&height.to_le_bytes());
        data.extend_from_slice(&[0; 10]);
        data
    }

    fn vp8l(width: u32, height: u32) -> Vec<u8> {
        let bits = (width - 1) | (height - 1) << 14;
        let mut data = vec![0x2f];
        data.extend_from_slice(&bits.to_le_bytes());
        data.extend_from_slice(&[0; 7]);
        data
    }

    fn animated(width: u32, height: u32) -> Vec<u8> {
        riff(&[
            (b"VP8X", vp8x(true, width, height)),
            (b"ANIM", vec![0; 6]),
            (b"ANMF", vec![0; 17]),
        ])
    }

    #[test]
    fn reads_every_webp_layout() {
        assert_eq!(
            inspect_webp(&animated(512, 512)),
            Ok(WebpInfo { width: 512, height: 512, animated: true })
        );
        assert_eq!(
            inspect_webp(&riff(&[(b"VP8 ", vp8(512, 512))])),
            Ok(WebpInfo { width: 512, height: 512, animated: false })
        );
        assert_eq!(
            inspect_webp(&riff(&[(b"VP8L", vp8l(512, 300))])),
            Ok(WebpInfo { width: 512, height: 300, animated: false })
        );
        let extended_static = riff(&[(b"VP8X", vp8x(false, 512, 512)), (b"VP8L", vp8l(512, 512))]);
        assert_eq!(
            inspect_webp(&extended_static),
            Ok(WebpInfo { width: 512, height: 512, animated: false })
        );
    }

    #[test]
    fn rejects_files_that_are_not_webp() {
        assert!(inspect_webp(b"\x89PNG\r\n\x1a\n not a webp at all, just padding").is_err());
        let mut truncated = animated(512, 512);
        truncated.truncate(truncated.len() - 3);
        assert!(inspect_webp(&truncated).is_err());
        assert!(inspect_webp(&riff(&[(b"VP8X", vp8x(true, 512, 512))])).is_err());
    }

    #[test]
    fn stickers_follow_whatsapp_rules() {
        assert!(validate_sticker(&animated(512, 512), true).is_ok());
        assert!(validate_sticker(&animated(512, 512), false).is_err());
        assert!(validate_sticker(&animated(256, 256), true).is_err());
        assert!(validate_sticker(&riff(&[(b"VP8 ", vp8(512, 512))]), false).is_ok());

        let mut too_big = riff(&[(b"VP8 ", vp8(512, 512))]);
        too_big.resize(STATIC_MAX_BYTES + 1, 0);
        assert!(validate_sticker(&too_big, false).is_err());
    }
}
