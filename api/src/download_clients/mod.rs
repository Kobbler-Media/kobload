pub mod debrid;
pub mod sabnzbd;
pub mod transmission;

mod registry;
mod types;

use axum::http::StatusCode;
use axum::response::IntoResponse;
use sqlx::PgPool;
use uuid::Uuid;

use crate::archive;
use crate::repositories::clients as clients_repo;
use crate::types::DownloadClientConfig;

pub use registry::{
    DefaultDownloadClientResolver, DownloadClientCapabilities, DownloadClientHandler,
    DownloadClientResolver,
};
pub use types::{DownloadClientError, PollResult, ScannedFile, get_extension};

pub use sabnzbd::{add_sabnzbd, poll_sabnzbd, remove_sabnzbd, test_sabnzbd};
pub use transmission::{
    add_transmission_bytes, add_transmission_url, poll_transmission, remove_transmission,
    test_transmission,
};

pub async fn select_download_client(
    pool: &PgPool,
    client_id: Option<Uuid>,
    protocol: &str,
) -> Result<DownloadClientConfig, axum::response::Response> {
    let result = if let Some(client_id) = client_id {
        clients_repo::get_enabled_client_by_id(pool, client_id).await
    } else {
        clients_repo::get_default_client_for_protocol(pool, protocol).await
    };

    match result {
        Ok(Some(client)) => Ok(client),
        Ok(None) => Err((
            StatusCode::BAD_REQUEST,
            format!("no enabled download client for protocol {protocol}"),
        )
            .into_response()),
        Err(err) => {
            tracing::warn!(error = %err, "failed to select download client");
            Err((StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response())
        }
    }
}

/// Expand archives in a file list, adding their contents as additional ScannedFiles.
///
/// Files inside archives use the `!` delimiter: `archive.zip!/inner/path.epub`
///
/// This function:
/// - Iteratively expands nested archives up to `MAX_NESTING_DEPTH` levels
/// - Detects split RAR archives and extracts them to list nested contents
/// - Applies safety limits to prevent zip bombs
pub async fn expand_archives(
    files: Vec<ScannedFile>,
    unrar_path: Option<&str>,
    sevenz_path: Option<&str>,
) -> Vec<ScannedFile> {
    use archive::limits::{MAX_FILES_PER_ARCHIVE, MAX_NESTING_DEPTH, MAX_TEMP_EXTRACTION_BYTES};

    let mut result = Vec::new();
    let mut pending = files;
    let mut depth = 0;

    while !pending.is_empty() && depth < MAX_NESTING_DEPTH {
        let mut next_pending = Vec::new();

        for file in pending {
            // Check if this is an archive
            if !archive::is_archive_extension(&file.extension) {
                result.push(file);
                continue;
            }

            // List archive contents
            let archive_files = archive::list_archive_contents(
                &file.absolute_path,
                &file.relative_path,
                unrar_path,
                sevenz_path,
            )
            .await;

            // Safety: limit files per archive
            if archive_files.len() > MAX_FILES_PER_ARCHIVE {
                tracing::warn!(
                    archive = %file.relative_path,
                    count = archive_files.len(),
                    "Archive exceeds max file limit, skipping expansion"
                );
                result.push(file);
                continue;
            }

            // Collect filenames for split RAR detection
            let inner_filenames: Vec<&str> = archive_files
                .iter()
                .map(|f| {
                    // Extract just the filename from the archive path
                    // e.g., "archive.zip!/subdir/file.r00" -> "file.r00"
                    f.relative_path
                        .rsplit_once(archive::ARCHIVE_DELIMITER)
                        .map(|(_, inner)| inner)
                        .unwrap_or(&f.relative_path)
                        .rsplit('/')
                        .next()
                        .unwrap_or(&f.relative_path)
                })
                .collect();

            let split_sets = archive::detect_split_rar_sets(&inner_filenames);

            // Process split RAR sets
            for set in &split_sets {
                // Estimate total size of the split set
                let total_size: i64 = set
                    .all_files()
                    .iter()
                    .filter_map(|name| {
                        archive_files
                            .iter()
                            .find(|f| f.relative_path.ends_with(name))
                    })
                    .map(|f| f.size)
                    .sum();

                if total_size as u64 > MAX_TEMP_EXTRACTION_BYTES {
                    tracing::warn!(
                        archive = %file.relative_path,
                        set_main = %set.main_file,
                        size = total_size,
                        "Split RAR set too large, skipping"
                    );
                    continue;
                }

                // Find the inner paths for all RAR volumes
                let inner_paths: Vec<&str> = set
                    .all_files()
                    .iter()
                    .filter_map(|name| {
                        archive_files
                            .iter()
                            .find(|f| f.relative_path.ends_with(name))
                            .map(|f| {
                                // Extract inner path after the archive delimiter
                                f.absolute_path
                                    .split_once(archive::ARCHIVE_DELIMITER)
                                    .map(|(_, inner)| inner)
                                    .unwrap_or(&f.absolute_path)
                            })
                    })
                    .collect();

                if inner_paths.is_empty() {
                    continue;
                }

                // Extract all volumes to temp and list the RAR contents
                match archive::extract_files_to_temp(
                    &file.absolute_path,
                    &inner_paths,
                    unrar_path,
                    sevenz_path,
                )
                .await
                {
                    Ok((temp_dir, extracted)) => {
                        // Find the main RAR in extracted files
                        let main_inner_path = inner_paths
                            .iter()
                            .find(|p| p.ends_with(&set.main_file))
                            .copied();

                        if let Some(main_path) = main_inner_path
                            && let Some(temp_rar_path) = extracted.get(main_path)
                        {
                            // Build prefix for the nested files
                            let prefix = format!(
                                "{}{}{}{}",
                                file.relative_path,
                                archive::ARCHIVE_DELIMITER,
                                set.main_file,
                                archive::ARCHIVE_DELIMITER
                            );

                            // List contents of the reassembled RAR
                            let nested_files = archive::list_rar_contents_by_path(
                                temp_rar_path,
                                &prefix,
                                unrar_path,
                            )
                            .await;

                            // Add split RAR volumes info to nested files for extraction
                            let volumes = set.volumes.clone();
                            let nested_files_with_volumes: Vec<ScannedFile> = nested_files
                                .into_iter()
                                .map(|mut f| {
                                    f.split_rar_volumes = Some(volumes.clone());
                                    // Fix the absolute path to include the outer archive
                                    f.absolute_path = format!(
                                        "{}{}{}{}{}",
                                        file.absolute_path,
                                        archive::ARCHIVE_DELIMITER,
                                        set.main_file,
                                        archive::ARCHIVE_DELIMITER,
                                        f.absolute_path
                                            .split(archive::ARCHIVE_DELIMITER)
                                            .last()
                                            .unwrap_or(&f.absolute_path)
                                    );
                                    f
                                })
                                .collect();

                            // Queue nested archives for further expansion, add others to result
                            for nf in nested_files_with_volumes {
                                if archive::is_nested_archive(&nf.relative_path) {
                                    next_pending.push(nf);
                                } else {
                                    result.push(nf);
                                }
                            }
                        }

                        // temp_dir is dropped here, cleaning up extracted files
                        drop(temp_dir);
                    }
                    Err(err) => {
                        tracing::warn!(
                            archive = %file.relative_path,
                            set_main = %set.main_file,
                            error = %err,
                            "Failed to extract split RAR volumes"
                        );
                    }
                }
            }

            // Collect the filenames that are part of split RAR sets (to exclude from direct listing)
            let split_rar_files: std::collections::HashSet<&str> =
                split_sets.iter().flat_map(|set| set.all_files()).collect();

            // Add non-split-RAR files to result or queue for further expansion
            for af in archive_files {
                // Skip files that are part of a split RAR set
                let filename = af
                    .relative_path
                    .rsplit_once(archive::ARCHIVE_DELIMITER)
                    .map(|(_, inner)| inner)
                    .unwrap_or(&af.relative_path)
                    .rsplit('/')
                    .next()
                    .unwrap_or(&af.relative_path);

                if split_rar_files.contains(filename) {
                    continue;
                }

                // Check if this is a nested archive that needs expansion
                if archive::is_nested_archive(&af.relative_path) {
                    next_pending.push(af);
                } else {
                    result.push(af);
                }
            }

            // Also include the archive itself in the result
            result.push(file);
        }

        pending = next_pending;
        depth += 1;
    }

    // Any remaining pending files (hit depth limit) are added as-is
    if !pending.is_empty() {
        tracing::warn!(
            remaining = pending.len(),
            "Max nesting depth reached, some archives not expanded"
        );
        result.extend(pending);
    }

    // Phase 2: Detect distributed split RARs (volumes spread across multiple archives)
    let distributed_sets = archive::detect_distributed_split_rars(&result);

    if !distributed_sets.is_empty() {
        tracing::info!(
            count = distributed_sets.len(),
            "Found distributed split RAR sets"
        );
    }

    for set in distributed_sets {
        // Safety check: estimate total size
        let total_size: i64 = set
            .all_paths()
            .iter()
            .filter_map(|path| result.iter().find(|f| f.absolute_path == *path))
            .map(|f| f.size)
            .sum();

        if total_size as u64 > MAX_TEMP_EXTRACTION_BYTES {
            tracing::warn!(
                base_name = %set.base_name,
                size = total_size,
                "Distributed split RAR set too large, skipping"
            );
            continue;
        }

        // Extract all volumes to temp
        let all_paths: Vec<&str> = set.all_paths();
        match archive::extract_from_multiple_archives_to_temp(&all_paths, unrar_path, sevenz_path)
            .await
        {
            Ok((temp_dir, extracted)) => {
                // Find the main RAR's temp path
                let main_filename = set
                    .main_file_path
                    .rsplit('/')
                    .next()
                    .and_then(|s| s.rsplit(archive::ARCHIVE_DELIMITER).next())
                    .unwrap_or(&set.main_file_path);
                let main_temp_path = extracted
                    .values()
                    .find(|p| p.file_name().and_then(|n| n.to_str()) == Some(main_filename));

                if let Some(main_temp_path) = main_temp_path {
                    // Build prefix: use the main file's path as the base
                    // e.g., "dir/archive.zip!/file.rar" becomes "dir/archive.zip!/file.rar!/"
                    let prefix = format!("{}{}", set.main_file_path, archive::ARCHIVE_DELIMITER);

                    // Get the relative path prefix from the main file
                    let rel_prefix = result
                        .iter()
                        .find(|f| f.absolute_path == set.main_file_path)
                        .map(|f| format!("{}{}", f.relative_path, archive::ARCHIVE_DELIMITER))
                        .unwrap_or_else(|| prefix.clone());

                    // List contents of the reassembled RAR
                    let nested_files =
                        archive::list_rar_contents_by_path(main_temp_path, &rel_prefix, unrar_path)
                            .await;

                    tracing::info!(
                        main_rar = %main_temp_path.display(),
                        nested_count = nested_files.len(),
                        "Listed contents of distributed split RAR"
                    );

                    // Collect volume filenames for extraction later
                    let volume_filenames: Vec<String> = set
                        .volume_paths
                        .iter()
                        .filter_map(|p| {
                            p.rsplit('/')
                                .next()
                                .and_then(|s| s.rsplit(archive::ARCHIVE_DELIMITER).next())
                                .map(|s| s.to_string())
                        })
                        .collect();

                    // Build the list of all source archive paths (main + volumes)
                    let distributed_sources: Vec<String> =
                        std::iter::once(set.main_file_path.clone())
                            .chain(set.volume_paths.clone())
                            .collect();

                    // Only proceed if we found files inside the RAR
                    if !nested_files.is_empty() {
                        // Remove the processed RAR volume files from result
                        // (they're container files, not useful content)
                        let paths_to_remove: std::collections::HashSet<&str> =
                            distributed_sources.iter().map(|s| s.as_str()).collect();
                        result.retain(|f| !paths_to_remove.contains(f.absolute_path.as_str()));

                        // Add the extracted files to result
                        for mut nf in nested_files {
                            // Set the absolute path to include the outer archive path
                            nf.absolute_path = format!(
                                "{}{}{}",
                                set.main_file_path,
                                archive::ARCHIVE_DELIMITER,
                                nf.absolute_path
                                    .split(archive::ARCHIVE_DELIMITER)
                                    .last()
                                    .unwrap_or(&nf.absolute_path)
                            );
                            // Store volume info for extraction
                            nf.split_rar_volumes = Some(volume_filenames.clone());
                            // Store full paths to all source archives for distributed extraction
                            nf.distributed_rar_sources = Some(distributed_sources.clone());

                            result.push(nf);
                        }
                    }
                }

                drop(temp_dir);
            }
            Err(err) => {
                tracing::warn!(
                    base_name = %set.base_name,
                    error = %err,
                    "Failed to extract distributed split RAR volumes"
                );
            }
        }
    }

    // Phase 3: Detect distributed numeric splits (.7z.001, .zip.001, etc. spread across archives)
    let distributed_numeric = archive::detect_distributed_numeric_splits(&result);

    if !distributed_numeric.is_empty() {
        tracing::info!(
            count = distributed_numeric.len(),
            "Found distributed numeric split sets"
        );
    }

    for set in distributed_numeric {
        // Safety check: estimate total size
        let total_size: i64 = set
            .all_paths()
            .iter()
            .filter_map(|path| result.iter().find(|f| f.absolute_path == *path))
            .map(|f| f.size)
            .sum();

        if total_size as u64 > MAX_TEMP_EXTRACTION_BYTES {
            tracing::warn!(
                base_name = %set.base_name,
                size = total_size,
                "Distributed numeric split set too large, skipping"
            );
            continue;
        }

        // Extract all volumes to temp, reassemble, and list contents
        let all_paths: Vec<&str> = set.all_paths();
        match archive::extract_from_multiple_archives_to_temp(&all_paths, unrar_path, sevenz_path)
            .await
        {
            Ok((temp_dir, extracted)) => {
                // Sort extracted files by volume number and reassemble
                let mut volume_paths: Vec<_> = extracted
                    .iter()
                    .filter_map(|(src, path)| {
                        let filename = src.rsplit('/').next().unwrap_or(src);
                        archive::parse_numeric_split(filename).map(|(_, num)| (num, path.clone()))
                    })
                    .collect();
                volume_paths.sort_by_key(|(num, _)| *num);

                if volume_paths.is_empty() {
                    continue;
                }

                // Concatenate all volumes
                let mut reassembled = Vec::new();
                for (_, path) in &volume_paths {
                    if let Ok(data) = std::fs::read(path) {
                        reassembled.extend(data);
                    }
                }

                // Write reassembled archive to temp
                let archive_ext = set.archive_type.as_deref().unwrap_or("bin");
                let reassembled_path = temp_dir.path().join(format!("reassembled.{archive_ext}"));
                if let Err(err) = std::fs::write(&reassembled_path, &reassembled) {
                    tracing::warn!(
                        base_name = %set.base_name,
                        error = %err,
                        "Failed to write reassembled numeric split"
                    );
                    continue;
                }

                // Build prefix for nested files
                let rel_prefix = result
                    .iter()
                    .find(|f| f.absolute_path == set.first_volume_path)
                    .map(|f| format!("{}{}", f.relative_path, archive::ARCHIVE_DELIMITER))
                    .unwrap_or_default();

                // List contents of reassembled archive
                let nested_files = archive::list_archive_contents(
                    reassembled_path.to_str().unwrap_or(""),
                    &rel_prefix,
                    unrar_path,
                    sevenz_path,
                )
                .await;

                // Build the list of all source archive paths
                let distributed_sources: Vec<String> =
                    std::iter::once(set.first_volume_path.clone())
                        .chain(set.other_volume_paths.clone())
                        .collect();

                // Only proceed if we found files inside the archive
                if !nested_files.is_empty() {
                    // Remove the processed split volume files from result
                    let paths_to_remove: std::collections::HashSet<&str> =
                        distributed_sources.iter().map(|s| s.as_str()).collect();
                    result.retain(|f| !paths_to_remove.contains(f.absolute_path.as_str()));

                    // Add the extracted files to result
                    for mut nf in nested_files {
                        // Set the absolute path to include the first volume's path
                        nf.absolute_path = format!(
                            "{}{}{}",
                            set.first_volume_path,
                            archive::ARCHIVE_DELIMITER,
                            nf.absolute_path
                                .split(archive::ARCHIVE_DELIMITER)
                                .last()
                                .unwrap_or(&nf.absolute_path)
                        );
                        // Store info for extraction
                        nf.distributed_numeric_sources = Some(distributed_sources.clone());
                        nf.numeric_split_type = set.archive_type.clone();

                        result.push(nf);
                    }
                }

                drop(temp_dir);
            }
            Err(err) => {
                tracing::warn!(
                    base_name = %set.base_name,
                    error = %err,
                    "Failed to extract distributed numeric split volumes"
                );
            }
        }
    }

    result
}
