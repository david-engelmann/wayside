//! The strict value reader the manifest is parsed through.

use serde::Deserialize;
use std::fmt;

/// A JSON value read by the strict manifest reader.
///
/// The reader is a hand written Visitor over serde_json whose
/// visit_map sees every key as it arrives and refuses a repeat
/// at any level, because serde_json alone keeps the last value
/// of a repeated key and says nothing, where the Python reader's
/// object pairs hook refused it. Numbers keep their integer
/// value at i128 width; a number with a fraction or an exponent
/// lands in the Float variant and fails every integer check, as
/// it did under the Python type checks.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum StrictValue {
    Null,
    Bool(bool),
    Int(i128),
    Float,
    Str(String),
    Array(Vec<StrictValue>),
    Object(Vec<(String, StrictValue)>),
}

impl<'de> serde::Deserialize<'de> for StrictValue {
    fn deserialize<D>(deserializer: D) -> std::result::Result<StrictValue, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct StrictVisitor;

        impl<'de> serde::de::Visitor<'de> for StrictVisitor {
            type Value = StrictValue;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a JSON value")
            }

            fn visit_bool<E>(self, value: bool) -> std::result::Result<StrictValue, E> {
                Ok(StrictValue::Bool(value))
            }

            fn visit_i64<E>(self, value: i64) -> std::result::Result<StrictValue, E> {
                Ok(StrictValue::Int(value as i128))
            }

            fn visit_u64<E>(self, value: u64) -> std::result::Result<StrictValue, E> {
                Ok(StrictValue::Int(value as i128))
            }

            fn visit_i128<E>(self, value: i128) -> std::result::Result<StrictValue, E>
            where
                E: serde::de::Error,
            {
                Ok(StrictValue::Int(value))
            }

            fn visit_f64<E>(self, _value: f64) -> std::result::Result<StrictValue, E> {
                Ok(StrictValue::Float)
            }

            fn visit_str<E>(self, value: &str) -> std::result::Result<StrictValue, E>
            where
                E: serde::de::Error,
            {
                Ok(StrictValue::Str(value.to_string()))
            }

            fn visit_string<E>(self, value: String) -> std::result::Result<StrictValue, E> {
                Ok(StrictValue::Str(value))
            }

            fn visit_none<E>(self) -> std::result::Result<StrictValue, E> {
                Ok(StrictValue::Null)
            }

            fn visit_unit<E>(self) -> std::result::Result<StrictValue, E> {
                Ok(StrictValue::Null)
            }

            fn visit_some<D>(self, deserializer: D) -> std::result::Result<StrictValue, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                StrictValue::deserialize(deserializer)
            }

            fn visit_seq<A>(self, mut seq: A) -> std::result::Result<StrictValue, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                let mut items = Vec::new();
                while let Some(item) = seq.next_element()? {
                    items.push(item);
                }
                Ok(StrictValue::Array(items))
            }

            fn visit_map<A>(self, mut map: A) -> std::result::Result<StrictValue, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut entries: Vec<(String, StrictValue)> = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    if entries.iter().any(|(seen, _)| *seen == key) {
                        return Err(serde::de::Error::custom(format!(
                            "wayside-duplicate-key\x1f{key}"
                        )));
                    }
                    let value = map.next_value()?;
                    entries.push((key, value));
                }
                Ok(StrictValue::Object(entries))
            }
        }

        deserializer.deserialize_any(StrictVisitor)
    }
}

pub(crate) fn duplicate_key_of(error_text: &str) -> Option<String> {
    let marker = "wayside-duplicate-key\x1f";
    let start = error_text.find(marker)? + marker.len();
    let rest = &error_text[start..];
    let end = rest.rfind(" at line ").unwrap_or(rest.len());
    Some(rest[..end].to_string())
}
