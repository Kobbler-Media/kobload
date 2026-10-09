mod common;

use common::TestServer;
use reqwest::StatusCode;
use serde_json::json;
use std::path::PathBuf;
use uuid::Uuid;

/// Path to test fixtures directory
fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

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

    let data = kobload_api::archive::extract_file(
        fixture.to_str().unwrap(),
        "kobload_test_file.txt",
        None,
        None,
    )
    .await
    .unwrap();

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

    let data = kobload_api::archive::extract_nested_file(&nested_path, None, None, None)
        .await
        .unwrap();

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
