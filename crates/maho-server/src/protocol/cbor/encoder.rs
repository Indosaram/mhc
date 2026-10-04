use super::{
    CborValue,
    options::{CborError, CborOptions, MAX_SAFE_INTEGER},
};

struct Writer {
    bytes: Vec<u8>,
    options: CborOptions,
}

impl Writer {
    fn write(&mut self, bytes: &[u8]) -> Result<(), CborError> {
        if bytes.len()
            > self
                .options
                .max_byte_length
                .saturating_sub(self.bytes.len())
        {
            return Err(CborError(format!(
                "CBOR byte length exceeds configured limit of {}",
                self.options.max_byte_length
            )));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn argument(&mut self, major: u8, value: u64) -> Result<(), CborError> {
        let prefix = major << 5;
        if value < 24 {
            self.write(&[prefix
                | u8::try_from(value).map_err(|_| CborError("Malformed CBOR argument".into()))?])
        } else if let Ok(v) = u8::try_from(value) {
            self.write(&[prefix | 24, v])
        } else if let Ok(v) = u16::try_from(value) {
            self.write(&[prefix | 25])?;
            self.write(&v.to_be_bytes())
        } else if let Ok(v) = u32::try_from(value) {
            self.write(&[prefix | 26])?;
            self.write(&v.to_be_bytes())
        } else {
            self.write(&[prefix | 27])?;
            self.write(&value.to_be_bytes())
        }
    }

    fn text(&mut self, text: &str) -> Result<(), CborError> {
        self.limit(text.len(), self.options.max_byte_length, "text string")?;
        self.argument(
            3,
            u64::try_from(text.len()).map_err(|_| CborError("Text length overflow".into()))?,
        )?;
        self.write(text.as_bytes())
    }

    fn limit(&self, value: usize, limit: usize, kind: &str) -> Result<(), CborError> {
        if value > limit {
            Err(CborError(format!(
                "CBOR {kind} length exceeds configured limit of {limit}"
            )))
        } else {
            Ok(())
        }
    }

    fn value(&mut self, value: &CborValue, depth: usize) -> Result<(), CborError> {
        if depth > self.options.max_depth {
            return Err(CborError(format!(
                "CBOR nesting depth exceeds configured limit of {}",
                self.options.max_depth
            )));
        }
        match value {
            CborValue::Null => self.write(&[0xf6]),
            CborValue::Bool(v) => self.write(&[if *v { 0xf5 } else { 0xf4 }]),
            CborValue::Integer(v) => {
                if !(-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(v) {
                    return Err(CborError(
                        "CBOR integers must be safe JavaScript integers".into(),
                    ));
                }
                if *v >= 0 {
                    self.argument(0, v.unsigned_abs())
                } else {
                    self.argument(1, v.unsigned_abs() - 1)
                }
            }
            CborValue::Float(v) => {
                if !v.is_finite() {
                    return Err(CborError("CBOR numbers must be finite".into()));
                }
                if v.fract() == 0.0 && v.abs() > 9_007_199_254_740_991.0 {
                    return Err(CborError(
                        "CBOR integers must be safe JavaScript integers".into(),
                    ));
                }
                if v.fract() == 0.0 && v.to_bits() != (-0.0f64).to_bits() {
                    let n = format!("{v:.0}").parse::<i64>().map_err(|_| {
                        CborError("CBOR integers must be safe JavaScript integers".into())
                    })?;
                    if n >= 0 {
                        self.argument(0, n.unsigned_abs())
                    } else {
                        self.argument(1, n.unsigned_abs() - 1)
                    }
                } else {
                    self.write(&[0xfb])?;
                    self.write(&v.to_be_bytes())
                }
            }
            CborValue::Bytes(v) => {
                self.limit(v.len(), self.options.max_byte_length, "byte string")?;
                self.argument(
                    2,
                    u64::try_from(v.len()).map_err(|_| CborError("Byte length overflow".into()))?,
                )?;
                self.write(v)
            }
            CborValue::Text(v) => self.text(v),
            CborValue::Array(v) => {
                self.limit(v.len(), self.options.max_container_length, "array")?;
                self.argument(
                    4,
                    u64::try_from(v.len())
                        .map_err(|_| CborError("Array length overflow".into()))?,
                )?;
                for item in v {
                    self.value(item, depth + 1)?;
                }
                Ok(())
            }
            CborValue::Map(v) => {
                self.limit(v.len(), self.options.max_container_length, "map")?;
                self.argument(
                    5,
                    u64::try_from(v.len()).map_err(|_| CborError("Map length overflow".into()))?,
                )?;
                for (key, item) in v {
                    self.text(key)?;
                    self.value(item, depth + 1)?;
                }
                Ok(())
            }
        }
    }
}

pub fn encode_cbor(value: &CborValue, options: CborOptions) -> Result<Vec<u8>, CborError> {
    let mut writer = Writer {
        bytes: Vec::new(),
        options: options.resolve()?,
    };
    writer.value(value, 0)?;
    Ok(writer.bytes)
}
