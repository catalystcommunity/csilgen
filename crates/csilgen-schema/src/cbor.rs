use thiserror::Error;

const MAX_DEPTH: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatWidth {
    Sixteen,
    ThirtyTwo,
    SixtyFour,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FloatValue {
    pub width: FloatWidth,
    /// The original IEEE 754 bits. Only the low 16 or 32 bits are used for the
    /// corresponding width.
    pub bits: u64,
}

impl FloatValue {
    pub fn as_f64(self) -> f64 {
        match self.width {
            FloatWidth::Sixteen => half_to_f64(self.bits as u16),
            FloatWidth::ThirtyTwo => f32::from_bits(self.bits as u32) as f64,
            FloatWidth::SixtyFour => f64::from_bits(self.bits),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SpannedValue {
    pub offset: usize,
    pub end_offset: usize,
    pub value: DiagnosticValue,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DiagnosticValue {
    Integer(i128),
    Float(FloatValue),
    Text(String),
    Bytes(Vec<u8>),
    Bool(bool),
    Null,
    Undefined,
    Simple(u8),
    Array(Vec<SpannedValue>),
    Map(Vec<(SpannedValue, SpannedValue)>),
    Tag {
        tag: u64,
        value: Box<SpannedValue>,
    },
    Decimal {
        exponent: i128,
        mantissa: i128,
    },
    Timestamp {
        original_tag: u64,
        value: Box<SpannedValue>,
    },
}

impl DiagnosticValue {
    pub fn shape(&self) -> &'static str {
        match self {
            Self::Integer(_) => "integer",
            Self::Float(_) => "float",
            Self::Text(_) => "text string",
            Self::Bytes(_) => "byte string",
            Self::Bool(_) => "boolean",
            Self::Null => "null",
            Self::Undefined => "undefined",
            Self::Simple(_) => "simple value",
            Self::Array(_) => "array",
            Self::Map(_) => "map",
            Self::Tag { .. } => "semantic tag",
            Self::Decimal { .. } => "decimal fraction",
            Self::Timestamp { .. } => "timestamp",
        }
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("CBOR decode error at byte {offset}: {message}")]
pub struct CborError {
    pub offset: usize,
    pub message: String,
}

pub fn decode(bytes: &[u8]) -> Result<SpannedValue, CborError> {
    let mut decoder = Decoder { bytes, offset: 0 };
    let value = decoder.value(0, false)?;
    if decoder.offset != bytes.len() {
        return Err(decoder.error("trailing bytes after the CBOR item"));
    }
    Ok(value)
}

struct Decoder<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl Decoder<'_> {
    fn value(&mut self, depth: usize, allow_break: bool) -> Result<SpannedValue, CborError> {
        if depth > MAX_DEPTH {
            return Err(self.error("nesting exceeds 64 values"));
        }
        let start = self.offset;
        let initial = self.byte()?;
        if initial == 0xff {
            return if allow_break {
                Err(self.error("internal break marker"))
            } else {
                Err(self.error("break marker outside an indefinite value"))
            };
        }
        let major = initial >> 5;
        let additional = initial & 0x1f;
        let value = match major {
            0 => DiagnosticValue::Integer(self.argument(additional)? as i128),
            1 => DiagnosticValue::Integer(-1 - self.argument(additional)? as i128),
            2 => DiagnosticValue::Bytes(self.byte_string(additional, depth)?),
            3 => DiagnosticValue::Text(self.text_string(additional, depth)?),
            4 => DiagnosticValue::Array(self.array(additional, depth)?),
            5 => DiagnosticValue::Map(self.map(additional, depth)?),
            6 => {
                let tag = self.argument(additional)?;
                let inner = self.value(depth + 1, false)?;
                semantic_tag(tag, inner)
            }
            7 => self.simple(additional)?,
            _ => unreachable!(),
        };
        Ok(SpannedValue {
            offset: start,
            end_offset: self.offset,
            value,
        })
    }

    fn argument(&mut self, additional: u8) -> Result<u64, CborError> {
        match additional {
            value @ 0..=23 => Ok(value as u64),
            24 => Ok(self.byte()? as u64),
            25 => Ok(u16::from_be_bytes(self.take_array()?) as u64),
            26 => Ok(u32::from_be_bytes(self.take_array()?) as u64),
            27 => Ok(u64::from_be_bytes(self.take_array()?)),
            31 => Err(self.error("indefinite length is not valid here")),
            _ => Err(self.error("reserved additional-information value")),
        }
    }

    fn length(&mut self, additional: u8) -> Result<Option<usize>, CborError> {
        if additional == 31 {
            return Ok(None);
        }
        let value = self.argument(additional)?;
        usize::try_from(value)
            .map(Some)
            .map_err(|_| self.error("declared length does not fit in memory"))
    }

    fn byte_string(&mut self, additional: u8, depth: usize) -> Result<Vec<u8>, CborError> {
        match self.length(additional)? {
            Some(length) => Ok(self.take(length)?.to_vec()),
            None => {
                let mut output = Vec::new();
                loop {
                    if self.peek()? == 0xff {
                        self.offset += 1;
                        break;
                    }
                    let initial = self.byte()?;
                    if initial >> 5 != 2 || initial & 0x1f == 31 {
                        return Err(self
                            .error("an indefinite byte string contains a non-byte-string chunk"));
                    }
                    let length = self.length(initial & 0x1f)?.expect("checked above");
                    let chunk = self.take(length)?;
                    output.extend_from_slice(chunk);
                    if depth > MAX_DEPTH {
                        return Err(self.error("nesting exceeds 64 values"));
                    }
                }
                Ok(output)
            }
        }
    }

    fn text_string(&mut self, additional: u8, depth: usize) -> Result<String, CborError> {
        match self.length(additional)? {
            Some(length) => {
                let offset = self.offset;
                let bytes = self.take(length)?;
                std::str::from_utf8(bytes)
                    .map(str::to_owned)
                    .map_err(|_| CborError {
                        offset,
                        message: "text string is not valid UTF-8".to_string(),
                    })
            }
            None => {
                let mut output = String::new();
                loop {
                    if self.peek()? == 0xff {
                        self.offset += 1;
                        break;
                    }
                    let initial = self.byte()?;
                    if initial >> 5 != 3 || initial & 0x1f == 31 {
                        return Err(self
                            .error("an indefinite text string contains a non-text-string chunk"));
                    }
                    let length = self.length(initial & 0x1f)?.expect("checked above");
                    let offset = self.offset;
                    let bytes = self.take(length)?;
                    let text = std::str::from_utf8(bytes).map_err(|_| CborError {
                        offset,
                        message: "text string is not valid UTF-8".to_string(),
                    })?;
                    output.push_str(text);
                    if depth > MAX_DEPTH {
                        return Err(self.error("nesting exceeds 64 values"));
                    }
                }
                Ok(output)
            }
        }
    }

    fn array(&mut self, additional: u8, depth: usize) -> Result<Vec<SpannedValue>, CborError> {
        let length = self.length(additional)?;
        if let Some(length) = length
            && length > self.remaining()
        {
            return Err(self.error("array count is larger than the remaining input"));
        }
        let mut values = Vec::new();
        match length {
            Some(length) => {
                for _ in 0..length {
                    values.push(self.value(depth + 1, false)?);
                }
            }
            None => loop {
                if self.peek()? == 0xff {
                    self.offset += 1;
                    break;
                }
                values.push(self.value(depth + 1, false)?);
            },
        }
        Ok(values)
    }

    fn map(
        &mut self,
        additional: u8,
        depth: usize,
    ) -> Result<Vec<(SpannedValue, SpannedValue)>, CborError> {
        let length = self.length(additional)?;
        if let Some(length) = length
            && length > self.remaining() / 2
        {
            return Err(self.error("map count is larger than the remaining input"));
        }
        let mut entries = Vec::new();
        match length {
            Some(length) => {
                for _ in 0..length {
                    let key = self.value(depth + 1, false)?;
                    let value = self.value(depth + 1, false)?;
                    entries.push((key, value));
                }
            }
            None => loop {
                if self.peek()? == 0xff {
                    self.offset += 1;
                    break;
                }
                let key = self.value(depth + 1, false)?;
                if self.peek()? == 0xff {
                    return Err(self.error("indefinite map has a key without a value"));
                }
                let value = self.value(depth + 1, false)?;
                entries.push((key, value));
            },
        }
        Ok(entries)
    }

    fn simple(&mut self, additional: u8) -> Result<DiagnosticValue, CborError> {
        match additional {
            0..=19 => Ok(DiagnosticValue::Simple(additional)),
            20 => Ok(DiagnosticValue::Bool(false)),
            21 => Ok(DiagnosticValue::Bool(true)),
            22 => Ok(DiagnosticValue::Null),
            23 => Ok(DiagnosticValue::Undefined),
            24 => {
                let value = self.byte()?;
                if value < 32 {
                    return Err(self.error("simple value uses a non-minimal encoding"));
                }
                Ok(DiagnosticValue::Simple(value))
            }
            25 => Ok(DiagnosticValue::Float(FloatValue {
                width: FloatWidth::Sixteen,
                bits: u16::from_be_bytes(self.take_array()?) as u64,
            })),
            26 => Ok(DiagnosticValue::Float(FloatValue {
                width: FloatWidth::ThirtyTwo,
                bits: u32::from_be_bytes(self.take_array()?) as u64,
            })),
            27 => Ok(DiagnosticValue::Float(FloatValue {
                width: FloatWidth::SixtyFour,
                bits: u64::from_be_bytes(self.take_array()?),
            })),
            31 => Err(self.error("break marker outside an indefinite value")),
            _ => Err(self.error("reserved simple value")),
        }
    }

    fn byte(&mut self) -> Result<u8, CborError> {
        let byte = *self
            .bytes
            .get(self.offset)
            .ok_or_else(|| self.error("truncated CBOR item"))?;
        self.offset += 1;
        Ok(byte)
    }

    fn peek(&self) -> Result<u8, CborError> {
        self.bytes
            .get(self.offset)
            .copied()
            .ok_or_else(|| self.error("truncated indefinite CBOR item"))
    }

    fn take(&mut self, length: usize) -> Result<&[u8], CborError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| self.error("declared length overflows the input offset"))?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| self.error("declared length is larger than the remaining input"))?;
        self.offset = end;
        Ok(bytes)
    }

    fn take_array<const N: usize>(&mut self) -> Result<[u8; N], CborError> {
        self.take(N)?
            .try_into()
            .map_err(|_| self.error("truncated CBOR argument"))
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.offset
    }

    fn error(&self, message: impl Into<String>) -> CborError {
        CborError {
            offset: self.offset,
            message: message.into(),
        }
    }
}

fn semantic_tag(tag: u64, value: SpannedValue) -> DiagnosticValue {
    if tag == 4
        && let DiagnosticValue::Array(parts) = &value.value
        && parts.len() == 2
        && let (DiagnosticValue::Integer(exponent), DiagnosticValue::Integer(mantissa)) =
            (&parts[0].value, &parts[1].value)
    {
        return DiagnosticValue::Decimal {
            exponent: *exponent,
            mantissa: *mantissa,
        };
    }
    if (tag == 0 && matches!(&value.value, DiagnosticValue::Text(_)))
        || (tag == 1
            && matches!(
                &value.value,
                DiagnosticValue::Integer(_) | DiagnosticValue::Float(_)
            ))
    {
        return DiagnosticValue::Timestamp {
            original_tag: tag,
            value: Box::new(value),
        };
    }
    DiagnosticValue::Tag {
        tag,
        value: Box::new(value),
    }
}

fn half_to_f64(bits: u16) -> f64 {
    let sign = ((bits >> 15) & 1) as u64;
    let exponent = ((bits >> 10) & 0x1f) as i32;
    let fraction = (bits & 0x03ff) as u64;
    let f64_bits = if exponent == 0 {
        if fraction == 0 {
            sign << 63
        } else {
            let leading = 63 - fraction.leading_zeros() as i32;
            let exponent_bits = (999 + leading) as u64;
            let normalized = fraction << (10 - leading);
            (sign << 63) | (exponent_bits << 52) | ((normalized & 0x03ff) << 42)
        }
    } else if exponent == 0x1f {
        (sign << 63) | (0x7ff << 52) | (fraction << 42)
    } else {
        (sign << 63) | (((exponent - 15 + 1023) as u64) << 52) | (fraction << 42)
    };
    f64::from_bits(f64_bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_special_values_and_semantic_tags() {
        let value = decode(&[
            0x85, 0xf9, 0x80, 0x00, 0xfa, 0x7f, 0x80, 0x00, 0x00, 0xfb, 0x7f, 0xf8, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x01, 0xc4, 0x82, 0x21, 0x19, 0x04, 0xd2, 0xc0, 0x74, b'2', b'0',
            b'2', b'6', b'-', b'0', b'8', b'-', b'3', b'1', b'T', b'0', b'0', b':', b'0', b'0',
            b':', b'0', b'0', b'Z',
        ])
        .unwrap();
        let DiagnosticValue::Array(values) = value.value else {
            panic!("expected array")
        };
        assert_eq!(values.len(), 5);
        let DiagnosticValue::Float(negative_zero) = values[0].value else {
            panic!("expected float")
        };
        assert_eq!(negative_zero.width, FloatWidth::Sixteen);
        assert_eq!(negative_zero.bits, 0x8000);
        let DiagnosticValue::Float(infinity) = values[1].value else {
            panic!("expected float")
        };
        assert!(infinity.as_f64().is_infinite());
        let DiagnosticValue::Float(nan) = values[2].value else {
            panic!("expected float")
        };
        assert_eq!(nan.bits, 0x7ff8_0000_0000_0001);
        assert!(matches!(
            values[3].value,
            DiagnosticValue::Decimal {
                exponent: -2,
                mantissa: 1234
            }
        ));
        assert!(matches!(
            values[4].value,
            DiagnosticValue::Timestamp {
                original_tag: 0,
                ..
            }
        ));
    }

    #[test]
    fn converts_half_precision_subnormal_values() {
        let value = decode(&[0xf9, 0x00, 0x01]).unwrap();
        let DiagnosticValue::Float(value) = value.value else {
            panic!("expected float")
        };
        assert_eq!(value.width, FloatWidth::Sixteen);
        assert_eq!(value.as_f64(), 2_f64.powi(-24));
    }

    #[test]
    fn rejects_hostile_lengths_depth_and_utf8() {
        for bytes in [
            &[0x9b, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00][..],
            &[0xbb, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00][..],
            &[0x5b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff][..],
            &[0x7b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff][..],
            &[0x83, 0x01, 0x02][..],
            &[0x61, 0xff][..],
        ] {
            assert!(decode(bytes).is_err(), "accepted {bytes:02x?}");
        }
        let mut nested = vec![0x81; 66];
        nested.push(0xf6);
        assert!(decode(&nested).is_err());
        assert!(decode(&[0x83, 0x01, 0x02, 0x03]).is_ok());
    }

    #[test]
    fn accepts_indefinite_values_and_non_text_map_keys() {
        let value = decode(&[
            0xbf, 0x82, 0x01, 0x02, 0x5f, 0x42, 0xaa, 0xbb, 0x41, 0xcc, 0xff, 0xff,
        ])
        .unwrap();
        assert!(matches!(value.value, DiagnosticValue::Map(_)));
    }

    #[test]
    fn preserves_large_integers_undefined_and_tag_one_timestamp() {
        let value = decode(&[
            0x84, 0x1b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x3b, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff, 0xff, 0xf7, 0xc1, 0x1a, 0x65, 0x53, 0xf1, 0x00,
        ])
        .unwrap();
        let DiagnosticValue::Array(values) = value.value else {
            panic!("expected array")
        };
        assert!(matches!(
            values[0].value,
            DiagnosticValue::Integer(18_446_744_073_709_551_615)
        ));
        assert!(matches!(
            values[1].value,
            DiagnosticValue::Integer(-18_446_744_073_709_551_616)
        ));
        assert!(matches!(values[2].value, DiagnosticValue::Undefined));
        assert!(matches!(
            values[3].value,
            DiagnosticValue::Timestamp {
                original_tag: 1,
                ..
            }
        ));
    }

    #[test]
    fn leaves_invalid_known_tags_as_generic_tags() {
        let tag_zero_integer = decode(&[0xc0, 0x01]).unwrap();
        let tag_one_text = decode(&[0xc1, 0x61, b'x']).unwrap();
        assert!(matches!(
            tag_zero_integer.value,
            DiagnosticValue::Tag { tag: 0, .. }
        ));
        assert!(matches!(
            tag_one_text.value,
            DiagnosticValue::Tag { tag: 1, .. }
        ));
    }
}
