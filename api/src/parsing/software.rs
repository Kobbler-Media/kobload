//! Software release name parsing.
//!
//! Handles:
//! - Standard: `App.Name.v2.0.Windows.x64-GROUP`
//! - Portable: `App.Name.Portable.v1.0`
//! - macOS: `App.Name.v1.0.macOS-GROUP`
//! - Linux: `App.Name.v1.0.Linux.x64-GROUP`

use regex::Regex;
use std::sync::LazyLock;

use super::common::clean_title;
use super::types::{ParsedMedia, SoftwareParsed};

// =============================================================================
// Regexes
// =============================================================================

/// Version pattern: v2.0, v1.2.3, Version 2.0
static VERSION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:v|version\.?)(?P<ver>\d+(?:\.\d+)*)").unwrap());

/// Platform patterns
static PLATFORM_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?P<platform>Windows|Win(?:32|64)?|macOS|Mac|OSX|Linux|Ubuntu|Debian|Android|iOS)\b").unwrap()
});

/// Architecture patterns
static ARCH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?P<arch>x86(?:_64)?|x64|amd64|arm(?:64)?|aarch64|ia32|ia64|i386|i686)\b")
        .unwrap()
});

/// Portable indicator
static PORTABLE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bPortable\b").unwrap());

/// Crack indicators
static CRACK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:Cracked|CRACK(?:ED)?|Keygen|Patch(?:ed)?|Activat(?:ed|or)|Registration|Serial)",
    )
    .unwrap()
});

/// Release group pattern
static GROUP_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"-([A-Za-z0-9]+)(?:\.[a-z]{2,4})?$").unwrap());

// =============================================================================
// Parsing
// =============================================================================

/// Parse a software release name.
pub fn parse_software(name: &str) -> (String, ParsedMedia, f64) {
    let version = extract_version(name);
    let platform = extract_platform(name);
    let architecture = extract_architecture(name);
    let is_portable = PORTABLE_RE.is_match(name);
    let is_cracked = CRACK_RE.is_match(name);

    // Clean up title
    let title = extract_software_title(name);

    let confidence = if version.is_some() { 0.85 } else { 0.6 };

    (
        title.clone(),
        ParsedMedia::Software(SoftwareParsed {
            title,
            version,
            platform,
            architecture,
            is_cracked,
            is_portable,
        }),
        confidence,
    )
}

/// Extract version number.
fn extract_version(name: &str) -> Option<String> {
    VERSION_RE
        .captures(name)
        .and_then(|c| c.name("ver"))
        .map(|m| m.as_str().to_string())
}

/// Extract platform.
fn extract_platform(name: &str) -> Option<String> {
    PLATFORM_RE
        .captures(name)
        .and_then(|c| c.name("platform"))
        .map(|m| normalize_platform(m.as_str()))
}

/// Normalize platform name.
fn normalize_platform(platform: &str) -> String {
    match platform.to_lowercase().as_str() {
        "windows" | "win" | "win32" | "win64" => "Windows",
        "macos" | "mac" | "osx" => "macOS",
        "linux" | "ubuntu" | "debian" => "Linux",
        "android" => "Android",
        "ios" => "iOS",
        _ => platform,
    }
    .to_string()
}

/// Extract architecture.
fn extract_architecture(name: &str) -> Option<String> {
    ARCH_RE
        .captures(name)
        .and_then(|c| c.name("arch"))
        .map(|m| normalize_arch(m.as_str()))
}

/// Normalize architecture name.
fn normalize_arch(arch: &str) -> String {
    match arch.to_lowercase().as_str() {
        "x64" | "x86_64" | "amd64" => "x64",
        "x86" | "ia32" | "i386" | "i686" => "x86",
        "arm64" | "aarch64" => "ARM64",
        "arm" => "ARM",
        "ia64" => "IA64",
        _ => arch,
    }
    .to_string()
}

/// Extract software title (remove version, platform, group, etc.).
fn extract_software_title(name: &str) -> String {
    // Remove release group
    let cleaned = GROUP_RE.replace(name, "");
    // Remove version
    let cleaned = VERSION_RE.replace_all(&cleaned, " ");
    // Remove platform
    let cleaned = PLATFORM_RE.replace_all(&cleaned, " ");
    // Remove architecture
    let cleaned = ARCH_RE.replace_all(&cleaned, " ");
    // Remove portable marker
    let cleaned = PORTABLE_RE.replace_all(&cleaned, " ");
    // Remove crack markers
    let cleaned = CRACK_RE.replace_all(&cleaned, " ");

    clean_title(&cleaned)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_software_with_version() {
        let (title, media, _) = parse_software("Adobe.Photoshop.v2024.1.0-GROUP");
        assert!(title.contains("Adobe") || title.contains("Photoshop"));

        if let ParsedMedia::Software(s) = media {
            assert_eq!(s.version.as_deref(), Some("2024.1.0"));
        } else {
            panic!("Expected Software");
        }
    }

    #[test]
    fn test_software_platform() {
        let (_, media, _) = parse_software("App.v1.0.Windows.x64-GROUP");

        if let ParsedMedia::Software(s) = media {
            assert_eq!(s.platform.as_deref(), Some("Windows"));
            assert_eq!(s.architecture.as_deref(), Some("x64"));
        } else {
            panic!("Expected Software");
        }
    }

    #[test]
    fn test_software_portable() {
        let (_, media, _) = parse_software("App.Portable.v1.0");

        if let ParsedMedia::Software(s) = media {
            assert!(s.is_portable);
        } else {
            panic!("Expected Software");
        }
    }

    #[test]
    fn test_software_macos() {
        let (_, media, _) = parse_software("App.v1.0.macOS-GROUP");

        if let ParsedMedia::Software(s) = media {
            assert_eq!(s.platform.as_deref(), Some("macOS"));
        } else {
            panic!("Expected Software");
        }
    }

    #[test]
    fn test_software_cracked() {
        let (_, media, _) = parse_software("App.v1.0.Cracked-GROUP");

        if let ParsedMedia::Software(s) = media {
            assert!(s.is_cracked);
        } else {
            panic!("Expected Software");
        }
    }

    #[test]
    fn test_software_linux() {
        let (_, media, _) = parse_software("App.v2.0.Linux.x64-GROUP");

        if let ParsedMedia::Software(s) = media {
            assert_eq!(s.platform.as_deref(), Some("Linux"));
            assert_eq!(s.architecture.as_deref(), Some("x64"));
        } else {
            panic!("Expected Software");
        }
    }
}
