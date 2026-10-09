//! YAML definition types matching Prowlarr/Jackett Cardigann format.

use serde::{de, Deserialize, Deserializer};
use std::collections::HashMap;
use std::fmt;

/// Top-level Cardigann YAML definition.
#[derive(Debug, Clone, Deserialize)]
pub struct Definition {
    pub id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub language: Option<String>,
    #[serde(rename = "type")]
    pub site_type: Option<String>,
    pub encoding: Option<String>,
    #[serde(default)]
    pub links: Vec<String>,
    #[serde(default)]
    pub legacylinks: Vec<String>,
    #[serde(default)]
    pub caps: Caps,
    pub login: Option<Login>,
    pub search: SearchDef,
    #[serde(default)]
    pub settings: Vec<SettingDef>,
    pub download: Option<DownloadDef>,
    // Extra fields we don't use but must accept
    #[serde(rename = "requestDelay")]
    pub request_delay: Option<serde_yaml::Value>,
    pub followredirect: Option<bool>,
    pub certificates: Option<serde_yaml::Value>,
    pub testlinktorrent: Option<bool>,
    pub replaces: Option<serde_yaml::Value>,
}

/// Capabilities section.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Caps {
    #[serde(default)]
    pub categorymappings: Vec<CategoryMapping>,
    #[serde(default)]
    pub categories: HashMap<String, String>,
    #[serde(default)]
    pub modes: HashMap<String, Vec<String>>,
    pub allowrawsearch: Option<bool>,
    pub allowtvsearchimdb: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CategoryMapping {
    pub id: YamlString,
    pub cat: String,
    pub desc: Option<String>,
    pub default: Option<bool>,
}

/// Login configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct Login {
    pub path: Option<String>,
    #[serde(rename = "method")]
    pub method: Option<String>,
    pub cookie: Option<String>,
    pub cookies: Option<serde_yaml::Value>,
    #[serde(default)]
    pub inputs: HashMap<String, YamlString>,
    pub selectorinputs: Option<serde_yaml::Value>,
    pub error: Option<Vec<SelectorBlock>>,
    pub selectors: Option<serde_yaml::Value>,
    pub test: Option<LoginTest>,
    pub form: Option<String>,
    pub captcha: Option<CaptchaDef>,
    pub headers: Option<serde_yaml::Value>,
    pub submitpath: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LoginTest {
    pub path: String,
    pub selector: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CaptchaDef {
    #[serde(rename = "type")]
    pub captcha_type: Option<String>,
    pub image: Option<String>,
    pub input: Option<String>,
}

/// Search definition.
#[derive(Debug, Clone, Deserialize)]
pub struct SearchDef {
    pub path: Option<String>,
    #[serde(default)]
    pub paths: Vec<SearchPathDef>,
    pub method: Option<String>,
    #[serde(default)]
    pub inputs: HashMap<String, YamlString>,
    pub rows: RowsDef,
    #[serde(default)]
    pub fields: HashMap<String, FieldDef>,
    pub headers: Option<HashMap<String, StringOrVec>>,
    pub keywordsfilters: Option<Vec<FilterDef>>,
    pub followredirect: Option<bool>,
    pub allowrawsearch: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SearchPathDef {
    pub path: String,
    #[serde(default)]
    pub categories: Vec<YamlString>,
    pub method: Option<String>,
    #[serde(default)]
    pub inputs: HashMap<String, YamlString>,
    pub followredirect: Option<bool>,
    pub categorydesc: Option<serde_yaml::Value>,
    pub inheritinputs: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RowsDef {
    pub selector: String,
    pub after: Option<i32>,
    #[serde(default)]
    pub filters: Vec<FilterDef>,
    pub attribute: Option<String>,
    pub count: Option<SelectorBlock>,
    pub dateheaders: Option<SelectorBlock>,
    pub multiple: Option<bool>,
    pub missingattribute: Option<String>,
    pub remove: Option<String>,
}

/// Field extraction definition.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum FieldDef {
    Simple(String),
    Complex(FieldDefComplex),
}

#[derive(Debug, Clone, Deserialize)]
pub struct FieldDefComplex {
    pub selector: Option<String>,
    pub text: Option<YamlString>,
    pub attribute: Option<String>,
    #[serde(default)]
    pub filters: Vec<FilterDef>,
    pub optional: Option<bool>,
    #[serde(rename = "case")]
    pub case: Option<HashMap<YamlString, YamlString>>,
    pub remove: Option<String>,
    pub default: Option<YamlString>,
}

/// Filter definition (used in field extraction pipeline).
#[derive(Debug, Clone, Deserialize)]
pub struct FilterDef {
    pub name: String,
    #[serde(default)]
    pub args: FilterArgs,
}

/// Filter arguments — either a single string or a list.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(untagged)]
pub enum FilterArgs {
    Single(YamlString),
    List(Vec<YamlString>),
    #[default]
    None,
}

impl FilterArgs {
    pub fn as_str(&self) -> &str {
        match self {
            FilterArgs::Single(s) => s.as_str(),
            FilterArgs::List(v) => v.first().map(|s| s.as_str()).unwrap_or(""),
            FilterArgs::None => "",
        }
    }

    pub fn as_list(&self) -> Vec<&str> {
        match self {
            FilterArgs::Single(s) => vec![s.as_str()],
            FilterArgs::List(v) => v.iter().map(|s| s.as_str()).collect(),
            FilterArgs::None => vec![],
        }
    }
}

/// A string or array of strings — coerces to a single string.
/// Used for HTTP headers where values can be `"foo"` or `["foo"]`.
#[derive(Debug, Clone)]
pub enum StringOrVec {
    Single(String),
    List(Vec<String>),
}

impl StringOrVec {
    pub fn as_str(&self) -> &str {
        match self {
            StringOrVec::Single(s) => s,
            StringOrVec::List(v) => v.first().map(|s| s.as_str()).unwrap_or(""),
        }
    }
}

impl<'de> Deserialize<'de> for StringOrVec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct Visitor;
        impl<'de> de::Visitor<'de> for Visitor {
            type Value = StringOrVec;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                write!(f, "a string or array of strings")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<StringOrVec, E> {
                Ok(StringOrVec::Single(v.to_string()))
            }
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<StringOrVec, A::Error> {
                let mut v = Vec::new();
                while let Some(s) = seq.next_element::<String>()? {
                    v.push(s);
                }
                Ok(StringOrVec::List(v))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

/// A YAML scalar (string, integer, float, bool) coerced to String.
/// Used throughout definitions where values can be strings or numbers.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct YamlString(pub String);

impl YamlString {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::ops::Deref for YamlString {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for YamlString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct Visitor;
        impl<'de> de::Visitor<'de> for Visitor {
            type Value = YamlString;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                write!(f, "a string, number, or boolean")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<YamlString, E> {
                Ok(YamlString(v.to_string()))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<YamlString, E> {
                Ok(YamlString(v.to_string()))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<YamlString, E> {
                Ok(YamlString(v.to_string()))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<YamlString, E> {
                Ok(YamlString(v.to_string()))
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<YamlString, E> {
                Ok(YamlString(v.to_string()))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

/// Selector block for error/test/etc.
#[derive(Debug, Clone, Deserialize)]
pub struct SelectorBlock {
    pub selector: Option<String>,
    pub text: Option<YamlString>,
    pub attribute: Option<String>,
    #[serde(default)]
    pub filters: Vec<FilterDef>,
}

/// Settings definition for user-configurable fields.
#[derive(Debug, Clone, Deserialize)]
pub struct SettingDef {
    pub name: String,
    #[serde(rename = "type")]
    pub setting_type: String,
    pub label: Option<String>,
    pub default: Option<YamlString>,
    pub options: Option<HashMap<String, String>>,
}

impl SettingDef {
    /// Returns a human-readable description for well-known info setting types
    /// that don't carry their own label/default text. These are part of the
    /// Cardigann/Prowlarr definition spec.
    pub fn info_description(&self) -> Option<&'static str> {
        match self.setting_type.as_str() {
            "info_flaresolverr" => Some(
                "This indexer is protected by Cloudflare and requires \
                 FlareSolverr to access it.",
            ),
            "info_cookie" => Some(
                "This indexer requires browser cookies for authentication. \
                 Extract cookies from your browser after logging in.",
            ),
            "info_useragent" => {
                Some("This indexer requires a browser User-Agent header for access.")
            }
            "info_category_8000" => {
                Some("This indexer uses category 8000+ for site-specific categories.")
            }
            _ => None,
        }
    }
}

/// Download configuration (for download link rewriting).
#[derive(Debug, Clone, Deserialize)]
pub struct DownloadDef {
    pub selector: Option<String>,
    pub attribute: Option<String>,
    #[serde(default)]
    pub filters: Vec<FilterDef>,
    pub method: Option<String>,
    pub before: Option<DownloadBeforeDef>,
    pub selectors: Option<serde_yaml::Value>,
    pub infohash: Option<serde_yaml::Value>,
    pub headers: Option<serde_yaml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DownloadBeforeDef {
    pub path: Option<String>,
    pub method: Option<String>,
    #[serde(default)]
    pub inputs: HashMap<String, YamlString>,
}
