mod common;

use common::TestServer;
use reqwest::StatusCode;
use serde_json::json;
use futures::StreamExt;
use std::io::Write;
use std::path::PathBuf;
use uuid::Uuid;

/// Path to test fixtures directory
fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

/// Drain an archive stream into memory (fine for test-sized entries).
async fn collect(mut stream: kobload_api::archive::EntryStream) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk.expect("stream chunk"));
    }
    out
}

/// Deterministic incompressible-ish payload larger than one stream chunk.
fn payload(len: usize) -> Vec<u8> {
    let mut state: u32 = 0x9e37_79b9;
    (0..len)
        .map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 24) as u8
        })
        .collect()
}

fn write_zip(path: &std::path::Path, entries: &[(&str, &[u8])]) {
    let mut writer = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, data) in entries {
        writer.start_file(*name, options).unwrap();
        writer.write_all(data).unwrap();
    }
    writer.finish().unwrap();
}

/// Upper bound on any single chunk an archive stream yields.
const MAX_CHUNK: usize = 64 * 1024;

/// Test that file-by-ID endpoint returns correct file content.
#[tokio::test]
async fn test_download_file_by_id() {
    let server = TestServer::start().await;
    let client = server.admin_client();

    let download_id = Uuid::now_v7();
    let file_id = Uuid::now_v7();

    // Create a temp file to serve
    let temp_dir = tempfile::tempdir().unwrap();
    let test_file = temp_dir.path().join("test.epub");
    std::fs::write(&test_file, b"fake epub content for testing").unwrap();

    // Insert download directly with files_json
    sqlx::query(
        r#"
        INSERT INTO downloads (id, source, protocol, name, download_url, callback_url, metadata, status, files_json, file_path)
        VALUES ($1, 'test', 'torrent', 'Test', 'http://example.com', 'http://callback', '{}'::jsonb, 'completed', $2::jsonb, $3)
        "#,
    )
    .bind(download_id)
    .bind(json!([{
        "id": file_id.to_string(),
        "absolute_path": test_file.to_string_lossy().to_string(),
        "relative_path": "test.epub",
        "size": 28,
        "extension": "epub",
    }]))
    .bind(test_file.to_string_lossy().to_string())
    .execute(&server.pool)
    .await
    .unwrap();

    // Download the file by ID
    let file_resp = client
        .get(&format!(
            "/api/v1/downloads/{}/files/{}",
            download_id, file_id
        ))
        .await;
    assert_eq!(
        file_resp.status,
        StatusCode::OK,
        "File download failed: {}",
        file_resp.text()
    );
    assert_eq!(file_resp.text(), "fake epub content for testing");
}

/// Test that file-by-ID returns 404 for unknown file ID.
#[tokio::test]
async fn test_download_file_by_id_not_found() {
    let server = TestServer::start().await;
    let client = server.admin_client();

    let download_id = Uuid::now_v7();

    // Insert download with no files
    sqlx::query(
        r#"
        INSERT INTO downloads (id, source, protocol, name, download_url, callback_url, metadata, status)
        VALUES ($1, 'test', 'torrent', 'Test', 'http://example.com', 'http://callback', '{}'::jsonb, 'completed')
        "#,
    )
    .bind(download_id)
    .execute(&server.pool)
    .await
    .unwrap();

    // Try to get a file that doesn't exist
    let file_resp = client
        .get(&format!(
            "/api/v1/downloads/{}/files/{}",
            download_id,
            Uuid::now_v7()
        ))
        .await;
    file_resp.assert_status(StatusCode::NOT_FOUND);
}

/// Test that repository get_file_by_id works correctly.
#[tokio::test]
async fn test_get_file_by_id_repository() {
    let server = TestServer::start().await;

    let temp_dir = tempfile::tempdir().unwrap();
    let test_file = temp_dir.path().join("test.epub");
    std::fs::write(&test_file, b"fake epub").unwrap();

    let download_id = Uuid::now_v7();
    let file_id = Uuid::now_v7();

    sqlx::query(
        r#"
        INSERT INTO downloads (id, source, protocol, name, download_url, callback_url, metadata, status, files_json, file_path)
        VALUES ($1, 'test', 'torrent', 'Test', 'http://example.com', 'http://callback', '{}'::jsonb, 'completed', $2::jsonb, $3)
        "#,
    )
    .bind(download_id)
    .bind(json!([{
        "id": file_id.to_string(),
        "absolute_path": test_file.to_string_lossy().to_string(),
        "relative_path": "test.epub",
        "size": 9,
        "extension": "epub",
    }]))
    .bind(test_file.to_string_lossy().to_string())
    .execute(&server.pool)
    .await
    .unwrap();

    let file_info =
        kobload_api::repositories::downloads::get_file_by_id(&server.pool, download_id, file_id)
            .await
            .unwrap();
    assert!(file_info.is_some(), "File should be found by ID");
    assert_eq!(
        file_info.unwrap().absolute_path,
        test_file.to_string_lossy().to_string()
    );

    let missing = kobload_api::repositories::downloads::get_file_by_id(
        &server.pool,
        download_id,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert!(missing.is_none(), "Non-existent file ID should return None");
}

// =============================================================================
// Archive extraction tests using real fixtures
// =============================================================================

/// Test listing contents of a plain ZIP archive.
#[tokio::test]
async fn test_list_plain_zip_contents() {
    let fixture = fixtures_dir().join("test_kobload_plain.zip");
    if !fixture.exists() {
        eprintln!("Skipping test: fixture not found at {:?}", fixture);
        return;
    }

    let contents = kobload_api::archive::list_archive_contents(
        fixture.to_str().unwrap(),
        "", // no prefix
        None,
        None,
    )
    .await;

    assert_eq!(
        contents.len(),
        1,
        "Should have 1 file, got: {:?}",
        contents
            .iter()
            .map(|f| &f.relative_path)
            .collect::<Vec<_>>()
    );
    assert!(
        contents[0].relative_path.contains("kobload_test_file.txt"),
        "Should contain test file, got: {}",
        contents[0].relative_path
    );
}

/// Test listing contents of a nested ZIP (ZIP inside ZIP).
#[tokio::test]
async fn test_list_nested_zip_contents() {
    let fixture = fixtures_dir().join("test_kobload_nested.zip");
    if !fixture.exists() {
        eprintln!("Skipping test: fixture not found at {:?}", fixture);
        return;
    }

    let contents = kobload_api::archive::list_archive_contents(
        fixture.to_str().unwrap(),
        "", // no prefix
        None,
        None,
    )
    .await;

    assert_eq!(contents.len(), 1, "Should have 1 file (inner.zip)");
    assert!(
        contents[0].relative_path.contains("inner.zip"),
        "Should contain inner.zip, got: {}",
        contents[0].relative_path
    );
}

/// Test extracting a file from a plain ZIP.
#[tokio::test]
async fn test_extract_from_plain_zip() {
    let fixture = fixtures_dir().join("test_kobload_plain.zip");
    if !fixture.exists() {
        eprintln!("Skipping test: fixture not found at {:?}", fixture);
        return;
    }

    let stream = kobload_api::archive::stream_file(
        fixture.to_str().unwrap(),
        "kobload_test_file.txt",
        None,
        None,
    )
    .await
    .unwrap();
    let data = collect(stream).await;

    let content = String::from_utf8(data).unwrap();
    assert!(
        content.contains("KOBLOAD_TEST_CONTENT"),
        "Should contain test content, got: {}",
        &content[..100.min(content.len())]
    );
}

/// Test listing numeric split archive inside ZIP.
#[tokio::test]
async fn test_list_numeric_split_in_zip() {
    let fixture = fixtures_dir().join("test_kobload_numeric_split.zip");
    if !fixture.exists() {
        eprintln!("Skipping test: fixture not found at {:?}", fixture);
        return;
    }

    let contents = kobload_api::archive::list_archive_contents(
        fixture.to_str().unwrap(),
        "", // no prefix
        None,
        None,
    )
    .await;

    // Should find all the split parts
    assert!(contents.len() >= 2, "Should have multiple split parts");

    let has_001 = contents
        .iter()
        .any(|f| f.relative_path.contains("test_archive.7z.001"));
    let has_002 = contents
        .iter()
        .any(|f| f.relative_path.contains("test_archive.7z.002"));

    assert!(has_001, "Should have .001 file");
    assert!(has_002, "Should have .002 file");
}

/// Test detecting numeric split sets from archive contents.
#[tokio::test]
async fn test_detect_numeric_splits_in_zip() {
    let fixture = fixtures_dir().join("test_kobload_numeric_split.zip");
    if !fixture.exists() {
        eprintln!("Skipping test: fixture not found at {:?}", fixture);
        return;
    }

    let contents = kobload_api::archive::list_archive_contents(
        fixture.to_str().unwrap(),
        "", // no prefix
        None,
        None,
    )
    .await;

    let filenames: Vec<&str> = contents.iter().map(|f| f.relative_path.as_str()).collect();
    let split_sets = kobload_api::archive::detect_numeric_split_sets(&filenames);

    assert_eq!(split_sets.len(), 1, "Should detect 1 split set");
    assert!(
        split_sets[0].first_volume.contains("test_archive.7z"),
        "First volume should contain test_archive.7z, got: {}",
        split_sets[0].first_volume
    );
    assert!(
        !split_sets[0].other_volumes.is_empty(),
        "Should have other volumes"
    );
}

/// Test detecting distributed numeric splits across multiple ZIPs.
#[tokio::test]
async fn test_detect_distributed_numeric_splits() {
    use kobload_api::download_clients::ScannedFile;

    let dist_001 = fixtures_dir().join("test_kobload_dist_001.zip");
    let dist_002 = fixtures_dir().join("test_kobload_dist_002.zip");

    if !dist_001.exists() || !dist_002.exists() {
        eprintln!("Skipping test: distributed fixtures not found");
        return;
    }

    let delim = kobload_api::archive::ARCHIVE_DELIMITER;

    // Simulate what expand_archives produces: files from different archives
    // The relative_path must contain the delimiter for detection to work
    let files = vec![
        ScannedFile {
            id: Uuid::now_v7(),
            absolute_path: format!("{}{}test_archive.7z.001", dist_001.display(), delim),
            relative_path: format!("test_kobload_dist_001.zip{}test_archive.7z.001", delim),
            size: 500,
            extension: "001".to_string(),
            split_rar_volumes: None,
            distributed_rar_sources: None,
            distributed_numeric_sources: None,
            numeric_split_type: None,
            score: None,
            classification: None,
            file_metadata: None,
            mime_type: None,
        },
        ScannedFile {
            id: Uuid::now_v7(),
            absolute_path: format!("{}{}test_archive.7z.002", dist_002.display(), delim),
            relative_path: format!("test_kobload_dist_002.zip{}test_archive.7z.002", delim),
            size: 500,
            extension: "002".to_string(),
            split_rar_volumes: None,
            distributed_rar_sources: None,
            distributed_numeric_sources: None,
            numeric_split_type: None,
            score: None,
            classification: None,
            file_metadata: None,
            mime_type: None,
        },
    ];

    let distributed = kobload_api::archive::detect_distributed_numeric_splits(&files);

    assert_eq!(
        distributed.len(),
        1,
        "Should detect 1 distributed split set"
    );
    assert!(
        !distributed[0].other_volume_paths.is_empty(),
        "Should have at least 1 other volume"
    );
}

/// Test nested extraction through archive layers.
#[tokio::test]
async fn test_extract_nested_file() {
    let fixture = fixtures_dir().join("test_kobload_nested.zip");
    if !fixture.exists() {
        eprintln!("Skipping test: fixture not found at {:?}", fixture);
        return;
    }

    // Path format: outer.zip!/inner.zip!/file.txt
    let nested_path = format!(
        "{}{}inner.zip{}kobload_test_file.txt",
        fixture.display(),
        kobload_api::archive::ARCHIVE_DELIMITER,
        kobload_api::archive::ARCHIVE_DELIMITER,
    );

    let stream = kobload_api::archive::stream_nested_file(&nested_path, None, None, None)
        .await
        .unwrap();
    let data = collect(stream).await;

    let content = String::from_utf8(data).unwrap();
    assert!(
        content.contains("KOBLOAD_TEST_CONTENT"),
        "Should contain test content from nested extraction"
    );
}

/// Test extracting files to temp directory.
#[tokio::test]
async fn test_extract_files_to_temp() {
    let fixture = fixtures_dir().join("test_kobload_numeric_split.zip");
    if !fixture.exists() {
        eprintln!("Skipping test: fixture not found at {:?}", fixture);
        return;
    }

    let (temp_dir, extracted) = kobload_api::archive::extract_files_to_temp(
        fixture.to_str().unwrap(),
        &["test_archive.7z.001", "test_archive.7z.002"],
        None,
        None,
    )
    .await
    .unwrap();

    assert!(
        extracted.contains_key("test_archive.7z.001"),
        "Should have extracted .001"
    );
    assert!(
        extracted.contains_key("test_archive.7z.002"),
        "Should have extracted .002"
    );

    // Verify files actually exist on disk
    for path in extracted.values() {
        assert!(path.exists(), "Extracted file should exist: {:?}", path);
    }

    // Temp dir still exists
    assert!(temp_dir.path().exists());
}

/// Regression: archive entries used to be read whole into a `Vec<u8>` before
/// being served, so streaming a multi-GB file out of an archive pinned that
/// much memory. Entries must now arrive as a sequence of bounded chunks.
#[tokio::test]
async fn test_stream_zip_entry_in_bounded_chunks() {
    let temp_dir = tempfile::tempdir().unwrap();
    let zip_path = temp_dir.path().join("big.zip");
    let data = payload(1024 * 1024 + 123);
    write_zip(&zip_path, &[("video.mkv", &data)]);

    let mut stream =
        kobload_api::archive::stream_file(zip_path.to_str().unwrap(), "video.mkv", None, None)
            .await
            .unwrap();

    let mut out = Vec::new();
    let mut chunks = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.unwrap();
        assert!(
            chunk.len() <= MAX_CHUNK,
            "chunk of {} bytes exceeds the {MAX_CHUNK} byte bound",
            chunk.len()
        );
        chunks += 1;
        out.extend_from_slice(&chunk);
    }

    assert!(chunks > 1, "entry should arrive in several chunks, got {chunks}");
    assert_eq!(out, data);
}

/// A missing entry must fail before any data is produced, so the route can
/// still answer 404 instead of a truncated 200.
#[tokio::test]
async fn test_stream_zip_missing_entry_errors_up_front() {
    let temp_dir = tempfile::tempdir().unwrap();
    let zip_path = temp_dir.path().join("small.zip");
    write_zip(&zip_path, &[("present.txt", b"hello")]);

    let result =
        kobload_api::archive::stream_file(zip_path.to_str().unwrap(), "absent.txt", None, None)
            .await;
    assert!(result.is_err(), "missing entry should be an open error");
}

#[tokio::test]
async fn test_stream_empty_zip_entry() {
    let temp_dir = tempfile::tempdir().unwrap();
    let zip_path = temp_dir.path().join("empty.zip");
    write_zip(&zip_path, &[("empty.txt", b"")]);

    let stream =
        kobload_api::archive::stream_file(zip_path.to_str().unwrap(), "empty.txt", None, None)
            .await
            .unwrap();
    assert!(collect(stream).await.is_empty());
}

/// Dropping the stream part-way (client disconnect) must not wedge the
/// extractor or leave later extractions blocked.
#[tokio::test]
async fn test_stream_dropped_midway_then_reopened() {
    let temp_dir = tempfile::tempdir().unwrap();
    let zip_path = temp_dir.path().join("big.zip");
    let data = payload(2 * 1024 * 1024);
    write_zip(&zip_path, &[("video.mkv", &data)]);
    let zip_str = zip_path.to_str().unwrap();

    let mut stream = kobload_api::archive::stream_file(zip_str, "video.mkv", None, None)
        .await
        .unwrap();
    let first = stream.next().await.unwrap().unwrap();
    assert!(!first.is_empty());
    drop(stream);

    let stream = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        kobload_api::archive::stream_file(zip_str, "video.mkv", None, None),
    )
    .await
    .expect("reopen should not hang")
    .unwrap();
    assert_eq!(collect(stream).await, data);
}

#[tokio::test]
async fn test_stream_tar_gz_entry() {
    let temp_dir = tempfile::tempdir().unwrap();
    let tar_path = temp_dir.path().join("pack.tar.gz");
    let data = payload(300 * 1024);

    {
        let encoder = flate2::write::GzEncoder::new(
            std::fs::File::create(&tar_path).unwrap(),
            flate2::Compression::fast(),
        );
        let mut builder = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "dir/book.epub", data.as_slice())
            .unwrap();
        builder.into_inner().unwrap().finish().unwrap();
    }

    let stream = kobload_api::archive::stream_file(
        tar_path.to_str().unwrap(),
        "dir/book.epub",
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(collect(stream).await, data);

    let missing = kobload_api::archive::stream_file(
        tar_path.to_str().unwrap(),
        "dir/nope.epub",
        None,
        None,
    )
    .await;
    assert!(missing.is_err());
}

#[tokio::test]
async fn test_stream_single_gz() {
    let temp_dir = tempfile::tempdir().unwrap();
    let gz_path = temp_dir.path().join("book.epub.gz");
    let data = payload(200 * 1024);

    let mut encoder = flate2::write::GzEncoder::new(
        std::fs::File::create(&gz_path).unwrap(),
        flate2::Compression::fast(),
    );
    encoder.write_all(&data).unwrap();
    encoder.finish().unwrap();

    let stream =
        kobload_api::archive::stream_file(gz_path.to_str().unwrap(), "book.epub", None, None)
            .await
            .unwrap();
    assert_eq!(collect(stream).await, data);
}

/// A file that isn't valid gzip fails at open, not mid-response.
#[tokio::test]
async fn test_stream_corrupt_gz_errors_up_front() {
    let temp_dir = tempfile::tempdir().unwrap();
    let gz_path = temp_dir.path().join("broken.epub.gz");
    std::fs::write(&gz_path, b"definitely not gzip").unwrap();

    let result =
        kobload_api::archive::stream_file(gz_path.to_str().unwrap(), "broken.epub", None, None)
            .await;
    assert!(result.is_err());
}

/// Nested levels are unpacked to temp files and the innermost entry streamed;
/// the temp files must survive until the stream is fully read.
#[tokio::test]
async fn test_stream_nested_zip_large_inner_entry() {
    let temp_dir = tempfile::tempdir().unwrap();
    let data = payload(512 * 1024);

    let inner_path = temp_dir.path().join("inner.zip");
    write_zip(&inner_path, &[("deep/file.bin", &data)]);
    let inner_bytes = std::fs::read(&inner_path).unwrap();

    let outer_path = temp_dir.path().join("outer.zip");
    write_zip(&outer_path, &[("inner.zip", &inner_bytes)]);

    let delim = kobload_api::archive::ARCHIVE_DELIMITER;
    let nested = format!("{}{delim}inner.zip{delim}deep/file.bin", outer_path.display());

    let mut stream = kobload_api::archive::stream_nested_file(&nested, None, None, None)
        .await
        .unwrap();

    // Yield between chunks so any premature temp-dir cleanup would bite.
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.unwrap();
        assert!(chunk.len() <= MAX_CHUNK);
        out.extend_from_slice(&chunk);
        tokio::task::yield_now().await;
    }
    assert_eq!(out, data);

    let missing = kobload_api::archive::stream_nested_file(
        &format!("{}{delim}inner.zip{delim}deep/missing.bin", outer_path.display()),
        None,
        None,
        None,
    )
    .await;
    assert!(missing.is_err());
}

#[tokio::test]
async fn test_concat_files_to_path_preserves_order() {
    let temp_dir = tempfile::tempdir().unwrap();
    let parts: Vec<Vec<u8>> = vec![payload(70 * 1024), b"middle".to_vec(), payload(10)];
    let mut paths = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        let path = temp_dir.path().join(format!("vol.{:03}", i + 1));
        std::fs::write(&path, part).unwrap();
        paths.push(path);
    }

    let dest = temp_dir.path().join("joined.bin");
    let written = kobload_api::archive::concat_files_to_path(&paths, &dest)
        .await
        .unwrap();

    let expected = parts.concat();
    assert_eq!(written, expected.len() as u64);
    assert_eq!(std::fs::read(&dest).unwrap(), expected);
}

/// End to end: the file-by-ID route serves an archive entry as a streamed body
/// with the full content, and still 404s for an entry that isn't there.
#[tokio::test]
async fn test_download_file_by_id_from_zip_streams_entry() {
    let server = TestServer::start().await;
    let client = server.admin_client();

    let temp_dir = tempfile::tempdir().unwrap();
    let zip_path = temp_dir.path().join("release.zip");
    let data = payload(256 * 1024 + 7);
    write_zip(&zip_path, &[("book.epub", &data)]);

    let download_id = Uuid::now_v7();
    let file_id = Uuid::now_v7();
    let missing_id = Uuid::now_v7();
    let delim = kobload_api::archive::ARCHIVE_DELIMITER;
    let zip_str = zip_path.to_string_lossy().to_string();

    sqlx::query(
        r#"
        INSERT INTO downloads (id, source, protocol, name, download_url, callback_url, metadata, status, files_json, file_path)
        VALUES ($1, 'test', 'torrent', 'Test', 'http://example.com', 'http://callback', '{}'::jsonb, 'completed', $2::jsonb, $3)
        "#,
    )
    .bind(download_id)
    .bind(json!([
        {
            "id": file_id.to_string(),
            "absolute_path": format!("{zip_str}{delim}book.epub"),
            "relative_path": format!("release.zip{delim}book.epub"),
            "size": data.len(),
            "extension": "epub",
        },
        {
            "id": missing_id.to_string(),
            "absolute_path": format!("{zip_str}{delim}gone.epub"),
            "relative_path": format!("release.zip{delim}gone.epub"),
            "size": 1,
            "extension": "epub",
        }
    ]))
    .bind(&zip_str)
    .execute(&server.pool)
    .await
    .unwrap();

    let resp = client
        .get(&format!("/api/v1/downloads/{download_id}/files/{file_id}"))
        .await;
    resp.assert_status(StatusCode::OK);
    assert_eq!(resp.bytes(), data.as_slice());

    let resp = client
        .get(&format!("/api/v1/downloads/{download_id}/files/{missing_id}"))
        .await;
    resp.assert_status(StatusCode::NOT_FOUND);
}
