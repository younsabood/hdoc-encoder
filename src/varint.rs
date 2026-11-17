use crate::HdocError;

/// Append an unsigned LEB128 value.
pub fn write_u32(mut value: u32, output: &mut Vec<u8>) {
    while value >= 0x80 {
        output.push((value as u8) | 0x80);
        value >>= 7;
    }
    output.push(value as u8);
}

/// Read an unsigned LEB128 value, rejecting values longer than five bytes.
pub fn read_u32(input: &[u8], cursor: &mut usize) -> Result<u32, HdocError> {
    let mut result = 0u32;
    for shift in (0..=28).step_by(7) {
        let byte = *input.get(*cursor).ok_or(HdocError::Truncated)?;
        *cursor += 1;
        if shift == 28 && byte & 0xf0 != 0 {
            return Err(HdocError::InvalidFormat("varint overflows u32".into()));
        }
        result |= u32::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(result);
        }
    }
    Err(HdocError::InvalidFormat("unterminated varint".into()))
}

/// Append a signed i32 using ZigZag followed by unsigned LEB128.
pub fn write_i32(value: i32, output: &mut Vec<u8>) {
    let zigzag = ((value << 1) ^ (value >> 31)) as u32;
    write_u32(zigzag, output);
}

/// Read a signed i32 encoded with ZigZag and unsigned LEB128.
pub fn read_i32(input: &[u8], cursor: &mut usize) -> Result<i32, HdocError> {
    let value = read_u32(input, cursor)?;
    Ok(((value >> 1) as i32) ^ (-((value & 1) as i32)))
}

#[cfg(test)]
mod tests {
    use super::{read_i32, read_u32, write_i32, write_u32};

    #[test]
    fn unsigned_round_trips_boundaries() {
        for value in [0, 1, 127, 128, 16_383, 16_384, u32::MAX] {
            let mut bytes = Vec::new();
            write_u32(value, &mut bytes);
            assert_eq!(read_u32(&bytes, &mut 0).unwrap(), value);
        }
    }

    #[test]
    fn signed_round_trips_boundaries() {
        for value in [i32::MIN, -65_536, -1, 0, 1, 65_535, i32::MAX] {
            let mut bytes = Vec::new();
            write_i32(value, &mut bytes);
            assert_eq!(read_i32(&bytes, &mut 0).unwrap(), value);
        }
    }
}
