use serde::de;

/// Deserialize a boolean from a bool, string ("true"/"false"), or integer (0/1).
///
/// Needed because the frontend sends checkbox values as strings in dynamic config.
pub fn deserialize_bool_lenient<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: de::Deserializer<'de>,
{
    struct BoolVisitor;
    impl<'de> de::Visitor<'de> for BoolVisitor {
        type Value = bool;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a boolean or boolean-like string")
        }
        fn visit_bool<E: de::Error>(self, v: bool) -> Result<bool, E> {
            Ok(v)
        }
        fn visit_str<E: de::Error>(self, v: &str) -> Result<bool, E> {
            match v.to_lowercase().as_str() {
                "true" | "1" | "yes" => Ok(true),
                _ => Ok(false),
            }
        }
        fn visit_i64<E: de::Error>(self, v: i64) -> Result<bool, E> {
            Ok(v != 0)
        }
        fn visit_u64<E: de::Error>(self, v: u64) -> Result<bool, E> {
            Ok(v != 0)
        }
    }
    deserializer.deserialize_any(BoolVisitor)
}

/// Deserialize a u32 from a number or numeric string (e.g. "30" → 30).
///
/// Needed because the frontend sends numeric form values as strings in dynamic config.
pub fn deserialize_u32_lenient<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: de::Deserializer<'de>,
{
    struct U32Visitor;
    impl<'de> de::Visitor<'de> for U32Visitor {
        type Value = u32;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a u32 or numeric string")
        }
        fn visit_u64<E: de::Error>(self, v: u64) -> Result<u32, E> {
            u32::try_from(v).map_err(|_| de::Error::custom(format!("{v} out of range for u32")))
        }
        fn visit_i64<E: de::Error>(self, v: i64) -> Result<u32, E> {
            u32::try_from(v).map_err(|_| de::Error::custom(format!("{v} out of range for u32")))
        }
        fn visit_str<E: de::Error>(self, v: &str) -> Result<u32, E> {
            v.parse::<u32>()
                .map_err(|_| de::Error::custom(format!("cannot parse \"{v}\" as u32")))
        }
    }
    deserializer.deserialize_any(U32Visitor)
}
