//! Core types for parsed release metadata.
//!
//! These types are defined in `nats_jobs` (koblibs) as the shared payload types.
//! Re-exported here so the rest of kobload can use them via `crate::parsing::*`.

pub use nats_jobs::{
    AudiobookParsed, BookParsed, GameParsed, MovieParsed, MusicParsed, ParsedMedia, ParsedQuality,
    ParsedRelease, ReleaseFlags, SoftwareParsed, SpecialType, SportParsed, TvParsed, XxxParsed,
};
