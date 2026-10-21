use crate::quality::{ParsedQuality, parse_resolution};

/// Within a tier, a release this much lower in score still wins against one that many
/// devices cannot play directly.
const COMPAT_MARGIN: f64 = 0.15;

const DISC_TOKENS: &[&str] = &["iso", "bdmv", "m2ts", "bd25", "bd50", "bdiso"];

pub struct Fit {
    /// Too small for what it claims to be: a fake, a sample, or a password-protected archive.
    pub suspect: bool,
    pub sort_score: f64,
}

pub fn assess(title: &str, quality: &ParsedQuality, size: i64, score: f64) -> Fit {
    let tokens: Vec<String> = title
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect();
    let has = |t: &str| tokens.iter().any(|x| x == t);

    let height = quality
        .resolution
        .as_deref()
        .and_then(parse_resolution)
        .unwrap_or(0);
    let floor: i64 = match height {
        h if h >= 2160 => 500_000_000,
        h if h >= 720 => 100_000_000,
        _ => 50_000_000,
    };
    let suspect = (size > 0 && size < floor) || has("sample");

    let dolby_vision_only =
        quality.is_dolby_vision && !quality.is_hdr10 && !quality.is_hdr10plus && !has("hdr");
    let hard_to_play = quality.codec.as_deref() == Some("av1")
        || quality.is_3d
        || dolby_vision_only
        || DISC_TOKENS.iter().any(|t| has(t));
    let sort_score = if hard_to_play {
        score - COMPAT_MARGIN * score.abs()
    } else {
        score
    };
    Fit {
        suspect,
        sort_score,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quality::parse_quality;

    fn fit(title: &str, size: i64) -> Fit {
        assess(title, &parse_quality(title), size, 10.0)
    }

    #[test]
    fn flags_fakes_and_hard_to_play_releases() {
        let cases = [
            ("Movie.2024.1080p.BluRay.x264-G", 4_000_000_000, false, 10.0),
            ("Movie.2024.1080p.BluRay.x264-G", 40_000_000, true, 10.0),
            ("Movie.2024.2160p.WEB.x265-G", 300_000_000, true, 10.0),
            ("Movie.2024.480p.DVDRip.x264-G", 300_000_000, false, 10.0),
            ("Movie.2024.1080p.Sample.x264-G", 4_000_000_000, true, 10.0),
            ("Movie.2024.1080p.BluRay.AV1-G", 4_000_000_000, false, 8.5),
            ("Movie.2024.2160p.DV.HEVC-G", 9_000_000_000, false, 8.5),
            (
                "Movie.2024.2160p.DV.HDR10.HEVC-G",
                9_000_000_000,
                false,
                10.0,
            ),
            ("Movie.2024.1080p.BluRay.ISO-G", 30_000_000_000, false, 8.5),
        ];
        for (title, size, suspect, score) in cases {
            let f = fit(title, size);
            assert_eq!(f.suspect, suspect, "{title}");
            assert!(
                (f.sort_score - score).abs() < 1e-9,
                "{title}: {}",
                f.sort_score
            );
        }
    }
}
