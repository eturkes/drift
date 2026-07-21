use std::{borrow::Cow, collections::HashSet, fmt};

use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};

/// Parse enough of one JSON value to reject ambiguous duplicate object keys.
///
/// Call this before typed deserialization. `serde_json` otherwise keeps the last
/// duplicate, while another consumer may keep the first.
pub fn ensure_unique_keys(bytes: &[u8]) -> serde_json::Result<()> {
    serde_json::from_slice::<UniqueValue>(bytes).map(|_| ())
}

struct UniqueValue;

impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(UniqueVisitor)
    }
}

struct UniqueVisitor;

impl<'de> Visitor<'de> for UniqueVisitor {
    type Value = UniqueValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value with unique object keys")
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
        Ok(UniqueValue)
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
        Ok(UniqueValue)
    }

    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
        Ok(UniqueValue)
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
        Ok(UniqueValue)
    }

    fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E> {
        Ok(UniqueValue)
    }

    fn visit_string<E>(self, _value: String) -> Result<Self::Value, E> {
        Ok(UniqueValue)
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(UniqueValue)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(UniqueValue)
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        UniqueValue::deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<UniqueValue>()?.is_some() {}
        Ok(UniqueValue)
    }

    fn visit_map<A>(self, mut object: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut keys = HashSet::new();
        while let Some(key) = object.next_key::<Cow<'de, str>>()? {
            if keys.contains(&key) {
                return Err(de::Error::custom(format!(
                    "duplicate JSON object key {key:?}"
                )));
            }
            keys.insert(key);
            object.next_value::<UniqueValue>()?;
        }
        Ok(UniqueValue)
    }
}

#[cfg(test)]
mod tests {
    use super::ensure_unique_keys;

    #[test]
    fn accepts_nested_unique_json() {
        ensure_unique_keys(br#"{"a":[{"b":1},null],"c":"x"}"#).unwrap();
    }

    #[test]
    fn rejects_nested_duplicate_keys() {
        let error = ensure_unique_keys(br#"{"a":{"b":1,"b":2}}"#).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("duplicate JSON object key \"b\"")
        );
    }
}
