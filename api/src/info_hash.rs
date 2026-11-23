// Indexers are untrusted, and an info hash becomes a release key that reaches URLs and paths.
pub fn valid(raw: &str) -> Option<String> {
    let hash = raw.trim();
    let hex = hash.len() == 40 && hash.bytes().all(|b| b.is_ascii_hexdigit());
    let base32 = hash.len() == 32
        && hash
            .bytes()
            .all(|b| b.is_ascii_alphabetic() || (b'2'..=b'7').contains(&b));
    (hex || base32).then(|| hash.to_ascii_lowercase())
}

pub fn from_magnet(magnet: &str) -> Option<String> {
    let query = magnet.split_once('?').map_or(magnet, |(_, query)| query);
    query.split('&').find_map(|part| {
        let (name, value) = part.split_at_checked(part.find('=')?)?;
        let value = value[1..].to_ascii_lowercase();
        let hash = value.strip_prefix("urn:btih:")?;
        name.eq_ignore_ascii_case("xt").then(|| valid(hash))?
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_hex_or_base32_hash_is_a_hash() {
        let hex = "45FA4233EF87C58F5F8B4817E4D50C9F5363CAEF";
        let base32 = "IX5EEM7PQ7CY6X4LJAL6JVIMT5JWHSXP";
        assert_eq!(valid(hex).as_deref(), Some(hex.to_lowercase().as_str()));
        assert_eq!(
            valid(base32).as_deref(),
            Some(base32.to_lowercase().as_str())
        );
        for bad in [
            "",
            "../../x",
            "abc",
            "../../../../../../../../../../../../etc/x",
            "45fa4233ef87c58f5f8b4817e4d50c9f5363cae/",
            "ix5eem7pq7cy6x4ljal6jvimt5jwhs0p",
        ] {
            assert_eq!(valid(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_magnet_gives_up_only_a_valid_hash() {
        let hash = "45fa4233ef87c58f5f8b4817e4d50c9f5363caef";
        assert_eq!(
            from_magnet(&format!("magnet:?dn=x&xt=urn:btih:{hash}&tr=y")).as_deref(),
            Some(hash)
        );
        assert_eq!(from_magnet("magnet:?xt=urn:btih:../../x"), None);
        assert_eq!(from_magnet("magnet:?dn=x"), None);
    }
}
