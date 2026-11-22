#[derive(Debug, PartialEq, Eq)]
pub enum Span {
    Full,
    Partial { start: u64, end: u64 },
    Unsatisfiable,
}

enum Spec {
    From(u64, Option<u64>),
    Suffix(u64),
}

// RFC 9110 §14.2: a Range in an unknown unit, or one we can't parse or don't serve
// (several ranges), is ignored and the whole representation is sent.
pub fn span(header: Option<&str>, len: u64) -> Span {
    let Some(spec) = header.and_then(|h| parse(h.trim())) else {
        return Span::Full;
    };
    match spec {
        Spec::From(start, _) if start >= len => Span::Unsatisfiable,
        Spec::From(start, last) => Span::Partial {
            start,
            end: last.map_or(len - 1, |l| l.min(len - 1)),
        },
        Spec::Suffix(0) => Span::Unsatisfiable,
        Spec::Suffix(_) if len == 0 => Span::Unsatisfiable,
        Spec::Suffix(n) => Span::Partial {
            start: len.saturating_sub(n),
            end: len - 1,
        },
    }
}

fn parse(header: &str) -> Option<Spec> {
    let spec = header.strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None;
    }
    let (first, last) = spec.split_once('-')?;
    let (first, last) = (first.trim(), last.trim());
    if first.is_empty() {
        return last.parse().ok().map(Spec::Suffix);
    }
    let start: u64 = first.parse().ok()?;
    let last = match last {
        "" => None,
        last => Some(last.parse::<u64>().ok().filter(|&l| l >= start)?),
    };
    Some(Spec::From(start, last))
}
