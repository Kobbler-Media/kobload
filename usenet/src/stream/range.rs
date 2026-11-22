#[derive(Debug, PartialEq, Eq)]
pub enum Span {
    Full,
    Partial { start: u64, end: u64 },
    Unsatisfiable,
}

pub fn span(header: Option<&str>, len: u64) -> Span {
    let Some(header) = header else {
        return Span::Full;
    };
    partial(header.trim(), len).map_or(Span::Unsatisfiable, |(start, end)| Span::Partial {
        start,
        end,
    })
}

fn partial(header: &str, len: u64) -> Option<(u64, u64)> {
    let spec = header.strip_prefix("bytes=")?;
    if spec.contains(',') || len == 0 {
        return None;
    }
    let (first, last) = spec.split_once('-')?;
    let (first, last) = (first.trim(), last.trim());
    if first.is_empty() {
        let suffix: u64 = last.parse().ok().filter(|&n| n > 0)?;
        return Some((len.saturating_sub(suffix), len - 1));
    }
    let start: u64 = first.parse().ok().filter(|&s| s < len)?;
    let end = match last {
        "" => len - 1,
        last => last
            .parse::<u64>()
            .ok()
            .filter(|&e| e >= start)?
            .min(len - 1),
    };
    Some((start, end))
}
