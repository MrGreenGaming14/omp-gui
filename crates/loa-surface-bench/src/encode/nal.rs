//! Annex B byte stream helpers: split into NAL units, spot keyframes.

use super::Codec;

/// The NAL units of an Annex B buffer, without start codes.
pub fn units(stream: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = stream;
    std::iter::from_fn(move || {
        let start = find_start(rest)?;
        let body = &rest[start..];
        let (unit, next) = match find_start_code(body) {
            Some((code_at, _)) => (&body[..code_at], &body[code_at..]),
            None => (body, &[][..]),
        };
        rest = next;
        Some(trim_trailing_zeros(unit))
    })
    .filter(|unit| !unit.is_empty())
}

/// Index just past the first start code.
fn find_start(stream: &[u8]) -> Option<usize> {
    find_start_code(stream).map(|(at, len)| at + len)
}

/// Position and length of the first `00 00 01` or `00 00 00 01`.
fn find_start_code(stream: &[u8]) -> Option<(usize, usize)> {
    let at = stream.windows(3).position(|w| w == [0, 0, 1])?;
    if at > 0 && stream[at - 1] == 0 { Some((at - 1, 4)) } else { Some((at, 3)) }
}

fn trim_trailing_zeros(unit: &[u8]) -> &[u8] {
    let end = unit.iter().rposition(|b| *b != 0).map_or(0, |i| i + 1);
    &unit[..end]
}

/// NAL unit type from the unit's first header byte(s).
pub fn unit_type(codec: Codec, unit: &[u8]) -> Option<u8> {
    let first = *unit.first()?;
    Some(match codec {
        Codec::H264 => first & 0x1f,
        Codec::Hevc => (first >> 1) & 0x3f,
    })
}

/// Whether the access unit holds a picture decodable on its own: an H.264
/// IDR slice (type 5) or an HEVC IRAP picture (types 16 to 23).
pub fn is_keyframe(codec: Codec, access_unit: &[u8]) -> bool {
    units(access_unit).filter_map(|u| unit_type(codec, u)).any(|t| match codec {
        Codec::H264 => t == 5,
        Codec::Hevc => (16..=23).contains(&t),
    })
}

/// Whether the access unit holds a coded picture at all.
pub fn has_picture(codec: Codec, access_unit: &[u8]) -> bool {
    units(access_unit).filter_map(|u| unit_type(codec, u)).any(|t| match codec {
        Codec::H264 => (1..=5).contains(&t),
        Codec::Hevc => t <= 31,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_mixed_start_codes_and_finds_the_idr() {
        // SPS (4 byte start code), PPS (3 byte), IDR slice.
        let au = [0, 0, 0, 1, 0x67, 0x42, 0, 0, 1, 0x68, 0xce, 0, 0, 0, 1, 0x65, 0x88, 0x84];
        let types: Vec<u8> = units(&au).filter_map(|u| unit_type(Codec::H264, u)).collect();
        assert_eq!(types, vec![7, 8, 5]);
        assert!(is_keyframe(Codec::H264, &au));

        let p_slice = [0, 0, 0, 1, 0x41, 0x9a, 0x02];
        assert!(!is_keyframe(Codec::H264, &p_slice));
        assert!(has_picture(Codec::H264, &p_slice));
    }

    #[test]
    fn hevc_irap_types_are_keyframes() {
        // VPS (32), IDR_W_RADL (19).
        let au = [0, 0, 0, 1, 0x40, 0x01, 0x0c, 0, 0, 0, 1, 0x26, 0x01, 0xaf];
        assert!(is_keyframe(Codec::Hevc, &au));
        // TRAIL_R (1).
        let trail = [0, 0, 0, 1, 0x02, 0x01, 0xd0];
        assert!(!is_keyframe(Codec::Hevc, &trail));
    }
}
