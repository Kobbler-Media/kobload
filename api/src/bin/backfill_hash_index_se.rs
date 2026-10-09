//! Backfill: parse existing `torrent_hash_index.original_title` and
//! `torrent_hash_files.filename` values and populate the `season` +
//! `episodes` columns introduced in migration 0074.
//!
//! Performance: uses bulk `UPDATE ... FROM unnest(...)` (one roundtrip per
//! batch) rather than per-row updates.
//!
//! Idempotent, cursor-paged so un-parseable rows don't block the advance.
//!
//! Usage:
//!   DATABASE_URL=postgres://... cargo run -p kobload-api --release --bin backfill-hash-index-se \
//!       -- [--dry-run] [--batch 10000] [--force] [--from <info_hash>] [--skip-index|--skip-files]

use std::sync::Arc;
use std::time::Instant;

use kobload_api::parsing::classify_filename;
use kobload_api::repositories::hash_index::derive_season_episodes;
use sqlx::postgres::PgPoolOptions;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let force = args.iter().any(|a| a == "--force");
    let skip_index = args.iter().any(|a| a == "--skip-index");
    let skip_files = args.iter().any(|a| a == "--skip-files");
    let batch_size: i64 = args
        .iter()
        .position(|a| a == "--batch")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(10_000);
    let start_cursor: String = args
        .iter()
        .position(|a| a == "--from")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_default();

    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://kobload:kobload@localhost/kobload".to_string());

    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&database_url)
        .await?;

    eprintln!("=== hash index season/episodes backfill ===");
    eprintln!("batch size: {batch_size}");
    eprintln!("dry run:    {dry_run}");
    eprintln!("force:      {force}");
    if !start_cursor.is_empty() {
        eprintln!("from:       {start_cursor}");
    }
    eprintln!();

    if !skip_index {
        backfill_index(&pool, batch_size, dry_run, force, start_cursor.clone()).await?;
    }
    if !skip_files {
        backfill_files(&pool, batch_size, dry_run, force).await?;
    }

    Ok(())
}

/// Encode Option<Vec<i16>> as the Postgres array literal text form
/// (`{1,2,3}` or NULL) for bulk-update via unnest.
fn episodes_to_pg_literal(episodes: Option<&[i16]>) -> Option<String> {
    let list = episodes?;
    let mut s = String::with_capacity(2 + list.len() * 4);
    s.push('{');
    for (i, e) in list.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&e.to_string());
    }
    s.push('}');
    Some(s)
}

async fn backfill_index(
    pool: &sqlx::PgPool,
    batch_size: i64,
    dry_run: bool,
    force: bool,
    start_cursor: String,
) -> Result<(), Box<dyn std::error::Error>> {
    // Filter rules:
    //   season IS NULL                  → never visited, must process
    //   season IS NOT NULL, episodes NULL → previously pack or collapsed range; re-parse
    //   both set                        → skip unless --force
    let predicate = if force {
        "TRUE"
    } else {
        "(season IS NULL OR episodes IS NULL)"
    };

    let count_sql = format!("SELECT COUNT(*) FROM torrent_hash_index WHERE {predicate}");
    let total_pending: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(count_sql))
        .fetch_one(pool)
        .await?;

    eprintln!("--- torrent_hash_index ---");
    eprintln!("pending rows: {total_pending}");
    if total_pending == 0 {
        eprintln!("nothing to do");
        return Ok(());
    }

    let select_sql: Arc<str> = format!(
        "SELECT info_hash, original_title
         FROM torrent_hash_index
         WHERE info_hash > $1 AND {predicate}
         ORDER BY info_hash LIMIT $2"
    )
    .into();

    let start = Instant::now();
    let mut processed: i64 = 0;
    let mut updated: i64 = 0;
    let mut last_log = 0i64;
    let mut cursor = start_cursor;

    loop {
        let rows: Vec<(String, String)> = sqlx::query_as(sqlx::AssertSqlSafe(select_sql.clone()))
            .bind(&cursor)
            .bind(batch_size)
            .fetch_all(pool)
            .await?;

        if rows.is_empty() {
            break;
        }

        let mut hashes: Vec<String> = Vec::with_capacity(rows.len());
        let mut seasons: Vec<Option<i16>> = Vec::with_capacity(rows.len());
        let mut episodes: Vec<Option<String>> = Vec::with_capacity(rows.len());
        for (hash, title) in &rows {
            let (season, eps) = derive_season_episodes(title);
            if season.is_none() && eps.is_none() && !force {
                continue;
            }
            hashes.push(hash.clone());
            seasons.push(season);
            episodes.push(episodes_to_pg_literal(eps.as_deref()));
        }

        if !dry_run && !hashes.is_empty() {
            // One UPDATE for the whole batch. COALESCE keeps existing season
            // if new parse returned NULL (unless --force, in which case we
            // overwrite with EXCLUDED via `u.season`/`u.ep_lit` directly).
            let assign = if force {
                "SET season = u.season,
                     episodes = u.ep_lit::smallint[]"
            } else {
                "SET season = COALESCE(h.season, u.season),
                     episodes = COALESCE(h.episodes, u.ep_lit::smallint[])"
            };
            let update_sql = format!(
                "UPDATE torrent_hash_index AS h
                 {assign}
                 FROM unnest($1::text[], $2::smallint[], $3::text[])
                      AS u(info_hash, season, ep_lit)
                 WHERE h.info_hash = u.info_hash"
            );
            let result = sqlx::query(sqlx::AssertSqlSafe(update_sql))
                .bind(&hashes)
                .bind(&seasons)
                .bind(&episodes)
                .execute(pool)
                .await?;
            updated += result.rows_affected() as i64;
        } else if dry_run {
            updated += hashes.len() as i64;
        }

        cursor = rows.last().unwrap().0.clone();
        processed += rows.len() as i64;

        if processed - last_log >= 100_000 {
            let elapsed = start.elapsed();
            let rate = processed as f64 / elapsed.as_secs_f64().max(0.001);
            eprintln!(
                "  index: {processed}/{total_pending} ({updated} updated) in {:.1}s — {rate:.0} rows/s, cursor={cursor}",
                elapsed.as_secs_f64()
            );
            last_log = processed;
        }

        if (rows.len() as i64) < batch_size {
            break;
        }
    }

    let elapsed = start.elapsed();
    eprintln!(
        "index done: processed {processed}, updated {updated}{} in {:.1}s",
        if dry_run { " (dry-run)" } else { "" },
        elapsed.as_secs_f64()
    );
    eprintln!();
    Ok(())
}

async fn backfill_files(
    pool: &sqlx::PgPool,
    batch_size: i64,
    dry_run: bool,
    force: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // For files: cursor is (info_hash, file_idx). New-rule filter:
    //   season IS NULL                  → never visited
    //   episodes IS NULL                → was pack/collapsed; re-classify
    //   both set                        → skip unless --force
    let predicate = if force {
        "TRUE"
    } else {
        "(season IS NULL OR episodes IS NULL)"
    };

    let count_sql = format!(
        "SELECT COUNT(*) FROM torrent_hash_files
         WHERE media_type = 'video' AND {predicate}"
    );
    let total_pending: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(count_sql))
        .fetch_one(pool)
        .await?;

    eprintln!("--- torrent_hash_files ---");
    eprintln!("pending rows: {total_pending}");
    if total_pending == 0 {
        eprintln!("nothing to do");
        return Ok(());
    }

    let select_sql: Arc<str> = format!(
        "SELECT info_hash, file_idx, filename
         FROM torrent_hash_files
         WHERE media_type = 'video'
           AND (info_hash, file_idx) > ($1, $2)
           AND {predicate}
         ORDER BY info_hash, file_idx
         LIMIT $3"
    )
    .into();

    let start = Instant::now();
    let mut processed: i64 = 0;
    let mut updated: i64 = 0;
    let mut last_log = 0i64;
    let mut cursor_hash: String = String::new();
    let mut cursor_idx: i32 = -1;

    loop {
        let rows: Vec<(String, i32, String)> =
            sqlx::query_as(sqlx::AssertSqlSafe(select_sql.clone()))
                .bind(&cursor_hash)
                .bind(cursor_idx)
                .bind(batch_size)
                .fetch_all(pool)
                .await?;

        if rows.is_empty() {
            break;
        }

        let mut hashes: Vec<String> = Vec::with_capacity(rows.len());
        let mut idxs: Vec<i32> = Vec::with_capacity(rows.len());
        let mut seasons: Vec<Option<i16>> = Vec::with_capacity(rows.len());
        let mut episodes: Vec<Option<String>> = Vec::with_capacity(rows.len());
        for (hash, idx, filename) in &rows {
            let (season, eps, _media) = classify_filename(filename);
            if season.is_none() && eps.is_none() && !force {
                continue;
            }
            hashes.push(hash.clone());
            idxs.push(*idx);
            seasons.push(season);
            episodes.push(episodes_to_pg_literal(eps.as_deref()));
        }

        if !dry_run && !hashes.is_empty() {
            let assign = if force {
                "SET season = u.season,
                     episodes = u.ep_lit::smallint[]"
            } else {
                "SET season = COALESCE(f.season, u.season),
                     episodes = COALESCE(f.episodes, u.ep_lit::smallint[])"
            };
            let update_sql = format!(
                "UPDATE torrent_hash_files AS f
                 {assign}
                 FROM unnest($1::text[], $2::int[], $3::smallint[], $4::text[])
                      AS u(info_hash, file_idx, season, ep_lit)
                 WHERE f.info_hash = u.info_hash AND f.file_idx = u.file_idx"
            );
            let result = sqlx::query(sqlx::AssertSqlSafe(update_sql))
                .bind(&hashes)
                .bind(&idxs)
                .bind(&seasons)
                .bind(&episodes)
                .execute(pool)
                .await?;
            updated += result.rows_affected() as i64;
        } else if dry_run {
            updated += hashes.len() as i64;
        }

        let last = rows.last().unwrap();
        cursor_hash = last.0.clone();
        cursor_idx = last.1;
        processed += rows.len() as i64;

        if processed - last_log >= 100_000 {
            let elapsed = start.elapsed();
            let rate = processed as f64 / elapsed.as_secs_f64().max(0.001);
            eprintln!(
                "  files: {processed}/{total_pending} ({updated} updated) in {:.1}s — {rate:.0} rows/s, cursor=({cursor_hash},{cursor_idx})",
                elapsed.as_secs_f64()
            );
            last_log = processed;
        }

        if (rows.len() as i64) < batch_size {
            break;
        }
    }

    let elapsed = start.elapsed();
    eprintln!(
        "files done: processed {processed}, updated {updated}{} in {:.1}s",
        if dry_run { " (dry-run)" } else { "" },
        elapsed.as_secs_f64()
    );
    Ok(())
}
