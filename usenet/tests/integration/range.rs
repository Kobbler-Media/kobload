use axum::http::{StatusCode, header};
use kob_usenet::vendor::testkit::LoopbackServer;
use usenet_proto::Purpose;

use crate::support::{Fixture, Harness, collect, output, token};

struct Case<'a> {
    range: Option<String>,
    status: StatusCode,
    content_range: Option<String>,
    body: &'a [u8],
}

fn case(
    range: Option<String>,
    status: StatusCode,
    content_range: Option<String>,
    body: &[u8],
) -> Case<'_> {
    Case {
        range,
        status,
        content_range,
        body,
    }
}

#[tokio::test]
async fn range_requests_get_exact_bytes_and_headers() {
    let fixture = Fixture::season();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::new(dir.path(), &nntp).await;
    let release = harness.import(&fixture, "idx:guid").await;
    let index = output(&release, "Show.S01E01.mkv");
    let token = token(&release.release_key, index, Purpose::Play);
    let data = fixture.data(0);
    let len = data.len();

    let cases = [
        case(None, StatusCode::OK, None, data),
        case(
            Some("bytes=100-".into()),
            StatusCode::PARTIAL_CONTENT,
            Some(format!("bytes 100-{}/{len}", len - 1)),
            &data[100..],
        ),
        case(
            Some("bytes=10-19".into()),
            StatusCode::PARTIAL_CONTENT,
            Some(format!("bytes 10-19/{len}")),
            &data[10..20],
        ),
        case(
            Some("bytes=-500".into()),
            StatusCode::PARTIAL_CONTENT,
            Some(format!("bytes {}-{}/{len}", len - 500, len - 1)),
            &data[len - 500..],
        ),
        case(
            Some(format!("bytes=70000-{}", len + 100)),
            StatusCode::PARTIAL_CONTENT,
            Some(format!("bytes 70000-{}/{len}", len - 1)),
            &data[70000..],
        ),
        case(
            Some(format!("bytes={len}-")),
            StatusCode::RANGE_NOT_SATISFIABLE,
            Some(format!("bytes */{len}")),
            &[],
        ),
        case(
            Some("bytes=20-10".into()),
            StatusCode::RANGE_NOT_SATISFIABLE,
            Some(format!("bytes */{len}")),
            &[],
        ),
        case(
            Some("bytes=0-1,5-6".into()),
            StatusCode::RANGE_NOT_SATISFIABLE,
            Some(format!("bytes */{len}")),
            &[],
        ),
    ];

    for Case {
        range,
        status,
        content_range,
        body: expected,
    } in cases
    {
        let label = format!("{range:?}");
        for method in ["GET", "HEAD"] {
            let response = harness.stream(method, &token, range.as_deref()).await;
            assert_eq!(response.status(), status, "{method} {label}");
            let headers = response.headers().clone();
            assert_eq!(
                headers
                    .get(header::CONTENT_RANGE)
                    .map(|v| v.to_str().unwrap().to_owned()),
                content_range,
                "{method} {label}"
            );
            assert_eq!(headers[header::ACCEPT_RANGES], "bytes", "{method} {label}");
            let (body, error) = collect(response).await;
            assert_eq!(error, None, "{method} {label}");
            if status == StatusCode::RANGE_NOT_SATISFIABLE {
                continue;
            }
            assert_eq!(
                headers[header::CONTENT_TYPE],
                "video/x-matroska",
                "{method} {label}"
            );
            assert_eq!(
                headers[header::CONTENT_LENGTH].to_str().unwrap(),
                expected.len().to_string(),
                "{method} {label}"
            );
            if method == "HEAD" {
                assert!(body.is_empty(), "{label}: HEAD has no body");
            } else {
                assert_eq!(
                    body.len(),
                    expected.len(),
                    "{label}: Content-Length is what was sent"
                );
                assert!(body == expected, "{label}: bytes match");
            }
        }
    }
}

#[tokio::test]
async fn a_bounded_range_fetches_no_articles_past_its_end() {
    let fixture = Fixture::season();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::new(dir.path(), &nntp).await;
    let release = harness.import(&fixture, "idx:guid").await;
    let index = output(&release, "Show.S01E01.mkv");
    let token = token(&release.release_key, index, Purpose::Play);
    let before = nntp.body_commands();

    let response = harness.stream("GET", &token, Some("bytes=10-19")).await;
    let (body, error) = collect(response).await;

    assert_eq!((body.as_slice(), error), (&fixture.data(0)[10..20], None));
    assert!(
        nntp.body_commands() - before <= 1,
        "fetched {}",
        nntp.body_commands() - before
    );
}
