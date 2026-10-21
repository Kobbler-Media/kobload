pub(crate) struct Language {
    /// ISO 639-1. Cantonese shares `zh` with Chinese: stream ranking treats them as one
    /// audio language, quality profiles keep them apart by `name`.
    pub code: &'static str,
    /// Lowercase English name, reported as `ParsedQuality::language`.
    pub name: &'static str,
    /// ISO 639-2 codes, accepted as preference spellings. Inside a release name they are
    /// trusted only when also listed in a tag field.
    pub codes: &'static [&'static str],
    /// Release-name tokens trusted by both the audio-track parser and quality profiles.
    pub tags: &'static [&'static str],
    /// Trusted only by the audio-track parser, which reads past the first year, episode or
    /// resolution token and so can afford tokens that are ambiguous across a whole title.
    pub stream_tags: &'static [&'static str],
    /// Trusted only by quality profiles, which filter on the audience a release is cut for:
    /// subtitle markers like VOSTFR, and codes the audio parser skips because they are
    /// common words in episode titles ("chi", "fin", "dan", "vie").
    pub profile_tags: &'static [&'static str],
}

impl Language {
    pub fn spellings(&self) -> impl Iterator<Item = &'static str> + '_ {
        [self.code, self.name]
            .into_iter()
            .chain(self.codes.iter().copied())
            .chain(self.tags.iter().copied())
            .chain(self.stream_tags.iter().copied())
            .chain(self.profile_tags.iter().copied())
    }
}

const fn lang(
    code: &'static str,
    name: &'static str,
    codes: &'static [&'static str],
    tags: &'static [&'static str],
) -> Language {
    Language {
        code,
        name,
        codes,
        tags,
        stream_tags: &[],
        profile_tags: &[],
    }
}

/// Order is quality-profile priority: when a title names several languages, the first row
/// to match wins.
pub(crate) const LANGUAGES: &[Language] = &[
    lang("en", "english", &["eng"], &["eng"]),
    Language {
        stream_tags: &["vff", "vfq", "vfi", "vf2"],
        profile_tags: &["vostfr", "subfrench", "fastsub"],
        ..lang(
            "fr",
            "french",
            &["fre", "fra"],
            &["fre", "fra", "truefrench"],
        )
    },
    Language {
        stream_tags: &["deutsch"],
        ..lang("de", "german", &["ger", "deu"], &["ger", "deu"])
    },
    Language {
        stream_tags: &["esp", "lat"],
        ..lang("es", "spanish", &["spa"], &["spa", "latino", "castellano"])
    },
    lang("it", "italian", &["ita"], &["ita"]),
    lang("ru", "russian", &["rus"], &["rus"]),
    lang("ko", "korean", &["kor"], &["kor"]),
    Language {
        stream_tags: &["jap"],
        ..lang("ja", "japanese", &["jpn"], &["jpn"])
    },
    Language {
        profile_tags: &["chi"],
        ..lang("zh", "chinese", &["chi", "zho"], &["chn", "mandarin"])
    },
    lang("zh", "cantonese", &[], &[]),
    lang("hi", "hindi", &["hin"], &["hin"]),
    Language {
        profile_tags: &["ara"],
        ..lang("ar", "arabic", &["ara"], &[])
    },
    Language {
        stream_tags: &["brazilian"],
        profile_tags: &["por"],
        ..lang("pt", "portuguese", &["por"], &[])
    },
    lang("nl", "dutch", &["dut", "nld"], &["nld", "flemish"]),
    lang("sv", "swedish", &["swe"], &["swe"]),
    Language {
        profile_tags: &["dan"],
        ..lang("da", "danish", &["dan"], &[])
    },
    Language {
        profile_tags: &["nor"],
        ..lang("no", "norwegian", &["nor"], &[])
    },
    Language {
        profile_tags: &["fin"],
        ..lang("fi", "finnish", &["fin"], &[])
    },
    lang("pl", "polish", &["pol"], &["pol"]),
    Language {
        profile_tags: &["ces"],
        ..lang("cs", "czech", &["cze", "ces"], &["cze"])
    },
    lang("hu", "hungarian", &["hun"], &["hun"]),
    lang("tr", "turkish", &["tur"], &["tur"]),
    Language {
        profile_tags: &["tha"],
        ..lang("th", "thai", &["tha"], &[])
    },
    Language {
        profile_tags: &["vie"],
        ..lang("vi", "vietnamese", &["vie"], &[])
    },
    Language {
        profile_tags: &["ind"],
        ..lang("id", "indonesian", &["ind"], &[])
    },
    Language {
        profile_tags: &["msa"],
        ..lang("ms", "malay", &["may", "msa"], &[])
    },
    Language {
        profile_tags: &["tgl"],
        ..lang("tl", "tagalog", &["tgl"], &[])
    },
    lang("he", "hebrew", &["heb"], &["heb"]),
    Language {
        stream_tags: &["gre"],
        profile_tags: &["ell"],
        ..lang("el", "greek", &["gre", "ell"], &[])
    },
    Language {
        stream_tags: &["rum"],
        profile_tags: &["ron"],
        ..lang("ro", "romanian", &["rum", "ron"], &[])
    },
    lang("bg", "bulgarian", &["bul"], &["bul"]),
    lang("uk", "ukrainian", &["ukr"], &["ukr"]),
    lang("ta", "tamil", &["tam"], &[]),
    lang("te", "telugu", &["tel"], &[]),
    Language {
        stream_tags: &["farsi"],
        ..lang("fa", "persian", &["per", "fas"], &[])
    },
];
