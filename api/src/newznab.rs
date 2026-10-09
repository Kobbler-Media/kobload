//! Newznab XML response helpers.

use crate::watch::Release;
use quick_xml::escape::escape;

pub fn build_caps(base_url: &str) -> String {
    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>");
    xml.push_str("<caps>");
    xml.push_str("<server version=\"1.0\" title=\"Kobload\" />");
    xml.push_str("<limits max=\"100\" default=\"100\" />");
    xml.push_str("<searching>");
    xml.push_str("<search available=\"yes\" supportedParams=\"q\" />");
    xml.push_str("<tv-search available=\"yes\" supportedParams=\"q,season,ep\" />");
    xml.push_str("<movie-search available=\"yes\" supportedParams=\"q,year\" />");
    xml.push_str("</searching>");
    xml.push_str("<categories>");
    xml.push_str("<category id=\"2000\" name=\"Movies\" />");
    xml.push_str("<category id=\"5000\" name=\"TV\" />");
    xml.push_str("<category id=\"7000\" name=\"Books\" />");
    xml.push_str("</categories>");
    xml.push_str(&format!(
        "<registration available=\"no\" open=\"no\" url=\"{}\" />",
        escape(base_url)
    ));
    xml.push_str("</caps>");
    xml
}

pub fn build_search_response(base_url: &str, releases: &[Release], offset: u32) -> String {
    let total = releases.len();
    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>");
    xml.push_str(
        "<rss version=\"2.0\" xmlns:newznab=\"http://www.newznab.com/DTD/2010/feeds/attributes/\">",
    );
    xml.push_str("<channel>");
    xml.push_str("<title>Kobload</title>");
    xml.push_str(&format!("<link>{}</link>", escape(base_url)));
    xml.push_str("<description>Kobload search results</description>");
    xml.push_str(&format!(
        "<newznab:response offset=\"{}\" total=\"{}\" />",
        offset, total
    ));

    for release in releases {
        xml.push_str("<item>");
        xml.push_str(&format!("<title>{}</title>", escape(&release.title)));
        xml.push_str(&format!("<guid>{}</guid>", escape(&release.guid)));
        if !release.publish_date.is_empty() {
            xml.push_str(&format!(
                "<pubDate>{}</pubDate>",
                escape(&release.publish_date)
            ));
        }

        if let Some(info_url) = release.info_url.as_ref() {
            xml.push_str(&format!("<link>{}</link>", escape(info_url)));
        }

        let enclosure_url = release
            .download_url
            .as_ref()
            .or(release.magnet_url.as_ref());
        if let Some(url) = enclosure_url {
            let content_type = if release.protocol == "torrent" {
                "application/x-bittorrent"
            } else {
                "application/x-nzb"
            };
            xml.push_str(&format!(
                "<enclosure url=\"{}\" length=\"{}\" type=\"{}\" />",
                escape(url),
                release.size,
                content_type
            ));
        }

        for category in &release.category_ids {
            xml.push_str(&format!(
                "<newznab:attr name=\"category\" value=\"{}\" />",
                category
            ));
        }
        if release.size > 0 {
            xml.push_str(&format!(
                "<newznab:attr name=\"size\" value=\"{}\" />",
                release.size
            ));
        }
        if let Some(files) = release.files {
            xml.push_str(&format!(
                "<newznab:attr name=\"files\" value=\"{}\" />",
                files
            ));
        }
        if let Some(ref info_hash) = release.info_hash {
            xml.push_str(&format!(
                "<newznab:attr name=\"infohash\" value=\"{}\" />",
                escape(info_hash)
            ));
        }
        if let Some(seeders) = release.seeders {
            xml.push_str(&format!(
                "<newznab:attr name=\"seeders\" value=\"{}\" />",
                seeders
            ));
        }
        if let Some(leechers) = release.leechers {
            xml.push_str(&format!(
                "<newznab:attr name=\"leechers\" value=\"{}\" />",
                leechers
            ));
        }
        if let Some(grabs) = release.grabs {
            xml.push_str(&format!(
                "<newznab:attr name=\"grabs\" value=\"{}\" />",
                grabs
            ));
        }
        if let Some(votes) = release.votes {
            xml.push_str(&format!(
                "<newznab:attr name=\"votes\" value=\"{}\" />",
                votes
            ));
        }
        if let Some(indexer_id) = release.indexer_id {
            xml.push_str(&format!(
                "<newznab:attr name=\"indexer\" value=\"{}\" />",
                indexer_id
            ));
        }
        if let Some(indexer_source_id) = release.indexer_source_id {
            xml.push_str(&format!(
                "<newznab:attr name=\"indexer_source\" value=\"{}\" />",
                indexer_source_id
            ));
        }

        xml.push_str("</item>");
    }

    xml.push_str("</channel></rss>");
    xml
}

pub fn normalize_offset(params: &std::collections::HashMap<String, String>) -> u32 {
    params
        .get("offset")
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0)
}

pub fn normalize_limit(
    params: &std::collections::HashMap<String, String>,
    default_limit: u32,
) -> u32 {
    params
        .get("limit")
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(default_limit)
}

pub fn normalize_query(params: &std::collections::HashMap<String, String>) -> Option<String> {
    params
        .get("q")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

// Reserved for future compat helpers.
