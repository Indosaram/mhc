use super::{
    CborValue,
    options::{CborError, CborOptions, MAX_SAFE_INTEGER},
};
use std::collections::HashSet;

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
    options: CborOptions,
}

impl Reader<'_> {
    fn bytes(&mut self, length: usize) -> Result<&[u8], CborError> {
        if length > self.bytes.len() - self.offset {
            return Err(CborError("Truncated CBOR payload".into()));
        }
        let start = self.offset;
        self.offset += length;
        Ok(&self.bytes[start..self.offset])
    }
    fn byte(&mut self) -> Result<u8, CborError> {
        Ok(self.bytes(1)?[0])
    }
    fn argument(&mut self, ai: u8) -> Result<u64, CborError> {
        let value = match ai {
            0..=23 => u64::from(ai),
            24 => u64::from(self.byte()?),
            25 => {
                let b = self.bytes(2)?;
                u64::from(u16::from_be_bytes([b[0], b[1]]))
            }
            26 => {
                let b = self.bytes(4)?;
                u64::from(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
            }
            27 => {
                let b = self.bytes(8)?;
                u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
            }
            31 => {
                return Err(CborError(
                    "Indefinite-length CBOR items are not supported".into(),
                ));
            }
            _ => return Err(CborError("Malformed CBOR additional information".into())),
        };
        if value > MAX_SAFE_INTEGER.unsigned_abs() {
            return Err(CborError(
                "Decoded CBOR integer or length is outside the safe range".into(),
            ));
        }
        Ok(value)
    }
    fn length(&mut self, ai: u8, kind: &str, limit: usize) -> Result<usize, CborError> {
        if ai == 31 {
            return Err(CborError(format!(
                "Indefinite-length CBOR {kind}s are not supported"
            )));
        }
        let length = usize::try_from(self.argument(ai)?)
            .map_err(|_| CborError("Decoded CBOR length is outside the safe range".into()))?;
        if length > limit {
            return Err(CborError(format!(
                "CBOR {kind} length exceeds configured limit of {limit}"
            )));
        }
        Ok(length)
    }
    fn item(&mut self, depth: usize) -> Result<CborValue, CborError> {
        if depth > self.options.max_depth {
            return Err(CborError(format!(
                "CBOR nesting depth exceeds configured limit of {}",
                self.options.max_depth
            )));
        }
        let initial = self.byte()?;
        let ai = initial & 31;
        Ok(match initial >> 5 {
            0 => {
                CborValue::Integer(i64::try_from(self.argument(ai)?).map_err(|_| {
                    CborError("Decoded CBOR integer is outside the safe range".into())
                })?)
            }
            1 => {
                let n = i64::try_from(self.argument(ai)?).map_err(|_| {
                    CborError("Decoded CBOR integer is outside the safe range".into())
                })?;
                if n >= MAX_SAFE_INTEGER {
                    return Err(CborError(
                        "Decoded CBOR integer is outside the safe range".into(),
                    ));
                }
                CborValue::Integer(-1 - n)
            }
            2 => {
                let n = self.length(ai, "byte string", self.options.max_byte_length)?;
                CborValue::Bytes(self.bytes(n)?.to_vec())
            }
            3 => {
                let n = self.length(ai, "text string", self.options.max_byte_length)?;
                CborValue::Text(
                    std::str::from_utf8(self.bytes(n)?)
                        .map_err(|_| CborError("CBOR text string contains invalid UTF-8".into()))?
                        .into(),
                )
            }
            4 => {
                let n = self.length(ai, "array", self.options.max_container_length)?;
                let mut result = Vec::new();
                for _ in 0..n {
                    result.push(self.item(depth + 1)?);
                }
                CborValue::Array(result)
            }
            5 => {
                let n = self.length(ai, "map", self.options.max_container_length)?;
                let mut result = Vec::new();
                let mut keys = HashSet::new();
                for _ in 0..n {
                    let CborValue::Text(key) = self.item(depth + 1)? else {
                        return Err(CborError("CBOR map keys must be strings".into()));
                    };
                    if !keys.insert(key.clone()) {
                        return Err(CborError("CBOR map contains a duplicate key".into()));
                    }
                    result.push((key, self.item(depth + 1)?));
                }
                CborValue::Map(result)
            }
            6 => return Err(CborError("CBOR tags are not supported".into())),
            7 => match ai {
                20 => CborValue::Bool(false),
                21 => CborValue::Bool(true),
                22 => CborValue::Null,
                27 => {
                    let b = self.bytes(8)?;
                    let v = f64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]);
                    if !v.is_finite() {
                        return Err(CborError("Decoded CBOR number must be finite".into()));
                    }
                    if v.fract() == 0.0 && v.abs() > 9_007_199_254_740_991.0 {
                        return Err(CborError(
                            "Decoded CBOR integer is outside the safe range".into(),
                        ));
                    }
                    CborValue::Float(v)
                }
                31 => return Err(CborError("CBOR break marker is not supported".into())),
                _ => {
                    return Err(CborError(
                        "Unsupported CBOR simple value or floating-point width".into(),
                    ));
                }
            },
            _ => return Err(CborError("Malformed CBOR major type".into())),
        })
    }
}

pub fn decode_cbor(bytes: &[u8], options: CborOptions) -> Result<CborValue, CborError> {
    let options = options.resolve()?;
    if bytes.len() > options.max_byte_length {
        return Err(CborError(format!(
            "CBOR byte length exceeds configured limit of {}",
            options.max_byte_length
        )));
    }
    let mut reader = Reader {
        bytes,
        offset: 0,
        options,
    };
    let value = reader.item(0)?;
    if reader.offset != bytes.len() {
        return Err(CborError("CBOR payload contains trailing data".into()));
    }
    Ok(value)
}
