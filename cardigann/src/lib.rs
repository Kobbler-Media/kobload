//! Cardigann YAML indexer definition engine.
//!
//! Parses Prowlarr/Jackett-compatible YAML definitions and executes
//! search/RSS queries against indexers.

mod definition;
mod engine;
mod filters;
mod login;
mod selector;
mod template;

#[cfg(test)]
mod tests;

pub use definition::{Definition, SettingDef};
pub use engine::{CardigannEngine, CardigannResult, SearchRequest, SearchType};

/// Parse a YAML definition string into a `Definition`.
pub fn load_definition(yaml: &str) -> Result<Definition, yaml_serde::Error> {
    let yaml = yaml.strip_prefix('\u{FEFF}').unwrap_or(yaml);
    yaml_serde::from_str(yaml)
}
