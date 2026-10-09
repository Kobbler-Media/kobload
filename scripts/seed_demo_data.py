#!/usr/bin/env python3
"""
Seed demo downloads for Kobload UI testing.
Creates API keys, quality profiles, and downloads in various statuses.

If a kobbler database container is available, it will query for wanted books,
movies, and TV episodes and use those IDs in the download metadata for proper
integration testing.

Environment variables for fixed API keys (optional):
  KOBLOAD_KOBBLER_API_KEY - API key for kobbler-demo (default: kob-kobbler-demo-key)
"""
import argparse
import hashlib
import json
import os
import subprocess
import sys
import uuid

# Default dev API key (fixed for easy configuration)
DEFAULT_KOBBLER_API_KEY = os.environ.get("KOBLOAD_KOBBLER_API_KEY", "kob-kobbler-demo-key")


def execute_sql(db_container, sql, db_user="kobload", db_name="kobload"):
    """Execute SQL via psql in the DB container. Returns stdout on success."""
    result = subprocess.run(
        ["docker", "exec", db_container, "psql", "-U", db_user, "-d", db_name, "-t", "-c", sql],
        capture_output=True, text=True
    )
    if result.returncode != 0:
        print(f"SQL error: {result.stderr}", file=sys.stderr)
        return None
    return result.stdout.strip()


def fetch_kobbler_wanted_books(kobbler_db_container):
    """Fetch wanted books from kobbler's database for realistic download data."""
    if not kobbler_db_container:
        return []

    sql = """
    SELECT b.id, b.title,
           COALESCE((SELECT string_agg(a.name, ', ') FROM book_authors ba
                     JOIN authors a ON a.id = ba.author_id
                     WHERE ba.book_id = b.id), 'Unknown') AS authors
    FROM books b
    WHERE b.want_status = 'wanted' AND b.is_in_library = FALSE
    ORDER BY b.updated_at DESC
    LIMIT 10;
    """

    result = execute_sql(kobbler_db_container, sql, db_user="kobbler", db_name="kobbler")
    if not result:
        return []

    books = []
    for line in result.strip().split('\n'):
        if not line.strip():
            continue
        parts = [p.strip() for p in line.split('|')]
        if len(parts) >= 3:
            books.append({
                "id": parts[0],
                "title": parts[1],
                "authors": parts[2]
            })
    return books


def fetch_kobbler_wanted_movies(kobbler_db_container):
    """Fetch wanted movies from kobbler's database."""
    if not kobbler_db_container:
        return []

    sql = """
    SELECT id, title, year FROM movies
    WHERE want_status = 'wanted'
    ORDER BY updated_at DESC
    LIMIT 10;
    """

    result = execute_sql(kobbler_db_container, sql, db_user="kobbler", db_name="kobbler")
    if not result:
        return []

    movies = []
    for line in result.strip().split('\n'):
        if not line.strip():
            continue
        parts = [p.strip() for p in line.split('|')]
        if len(parts) >= 3:
            movies.append({
                "id": parts[0],
                "title": parts[1],
                "year": parts[2],
            })
    return movies


def fetch_kobbler_wanted_episodes(kobbler_db_container):
    """Fetch wanted TV episodes from kobbler's database."""
    if not kobbler_db_container:
        return []

    sql = """
    SELECT e.id, s.show_id, sh.title, s.id as season_id,
           s.season_identifier, e.episode_identifier
    FROM tv_episodes e
    JOIN tv_seasons s ON s.id = e.season_id
    JOIN tv_shows sh ON sh.id = s.show_id
    WHERE e.want_status = 'wanted'
    ORDER BY e.updated_at DESC
    LIMIT 10;
    """

    result = execute_sql(kobbler_db_container, sql, db_user="kobbler", db_name="kobbler")
    if not result:
        return []

    episodes = []
    for line in result.strip().split('\n'):
        if not line.strip():
            continue
        parts = [p.strip() for p in line.split('|')]
        if len(parts) >= 6:
            episodes.append({
                "episode_id": parts[0],
                "show_id": parts[1],
                "show_title": parts[2],
                "season_id": parts[3],
                "season_identifier": parts[4],
                "episode_identifier": parts[5],
            })
    return episodes


def hash_api_key(key):
    """Hash an API key the same way kobload does (SHA256)."""
    return hashlib.sha256(key.encode()).hexdigest()


def seed_api_keys(db_container):
    """Create kobbler-demo API key, or use existing one."""
    name = "kobbler-demo"

    # First check if key with this name already exists
    fetch_sql = f"SELECT id, key FROM api_keys WHERE name = '{name}' AND revoked_at IS NULL LIMIT 1;"
    result = execute_sql(db_container, fetch_sql)

    if result and result.strip():
        # Key exists, use it
        parts = result.strip().split("|")
        if len(parts) >= 2:
            existing_id = parts[0].strip()
            existing_key = parts[1].strip() if parts[1].strip() else None
            if existing_key:
                print(f"  Using existing API key: {name} = {existing_key}")
                return {"id": existing_id, "name": name, "key": existing_key}
            else:
                print(f"  Found API key '{name}' but key value not stored (old backup?)")

    # Create new key with fixed dev value
    key_id = str(uuid.uuid4())
    raw_key = DEFAULT_KOBBLER_API_KEY
    key_hash = hash_api_key(raw_key)

    sql = f"""
    INSERT INTO api_keys (id, name, key_hash, key, created_at)
    VALUES (
        '{key_id}',
        '{name}',
        '{key_hash}',
        '{raw_key}',
        NOW()
    );
    """

    result = execute_sql(db_container, sql)
    if result is not None:
        print(f"  Created API key: {name} = {raw_key}")
        return {"id": key_id, "name": name, "key": raw_key}

    return None


def get_media_type_ids(db_container):
    """Look up media_type_config IDs for the media types we need."""
    media_types = {}
    for key in ["ebook", "movies", "tv"]:
        sql = f"SELECT id FROM media_type_config WHERE key = '{key}';"
        result = execute_sql(db_container, sql)
        if result and result.strip():
            media_types[key] = result.strip().split()[0]
    return media_types


def seed_quality_profiles(db_container):
    """Create demo quality profiles for each media kind."""
    profiles = {}

    items = [
        ("Default Ebook", "book"),
        ("HD Movies", "movie"),
        ("HD TV", "tv"),
    ]

    for name, media_kind in items:
        fetch_sql = f"SELECT id FROM quality_profiles WHERE name = '{name}';"
        result = execute_sql(db_container, fetch_sql)
        if result and result.strip():
            profiles[name] = result.strip().split()[0]
            continue

        profile_id = str(uuid.uuid4())
        sql = f"""
        INSERT INTO quality_profiles (id, name, media_kind, created_at, updated_at)
        VALUES (
            '{profile_id}',
            '{name}',
            '{media_kind}',
            NOW(),
            NOW()
        );
        """
        result = execute_sql(db_container, sql)
        if result is not None:
            profiles[name] = profile_id

    return profiles


def seed_downloads(db_container, api_key, kobbler_books=None, kobbler_movies=None, kobbler_episodes=None):
    """Create demo downloads in various statuses.

    Accepts books, movies, and TV episodes from kobbler for realistic metadata.
    """
    if not api_key:
        print("  No API key available, skipping downloads")
        return 0

    # Define download statuses to cycle through
    statuses = [
        {"status": "downloading", "progress": 45.5},
        {"status": "downloading", "progress": 72.0},
        {"status": "pending", "progress": None},
        {"status": "completed", "progress": 100.0},
        {"status": "processing", "progress": 100.0},
        {"status": "failed", "progress": 23.0, "error": "Torrent stalled - no seeds available"},
        {"status": "imported", "progress": 100.0},
    ]

    downloads = []

    # Book downloads
    if kobbler_books:
        for i, book in enumerate(kobbler_books):
            status_info = statuses[i % len(statuses)]
            protocol = "torrent" if i % 2 == 0 else "usenet"
            metadata = json.dumps({
                "book_id": book["id"],
                "provider": "hardcover",
                "title": book["title"]
            })
            downloads.append({
                "name": f"{book['authors']} - {book['title']}",
                "protocol": protocol,
                "media_kind": "book",
                "metadata": metadata,
                **status_info
            })
        print(f"  Using {len(kobbler_books)} wanted books from kobbler")
    else:
        print("  Using demo book data (no kobbler books found)")
        downloads.extend([
            {
                "name": "Brandon Sanderson - Mistborn The Final Empire",
                "protocol": "torrent",
                "media_kind": "book",
                "status": "downloading",
                "progress": 45.5,
                "metadata": '{"book_id": "demo-001", "provider": "demo", "title": "Mistborn: The Final Empire"}',
            },
            {
                "name": "Patrick Rothfuss - The Name of the Wind",
                "protocol": "usenet",
                "media_kind": "book",
                "status": "downloading",
                "progress": 72.0,
                "metadata": '{"book_id": "demo-002", "provider": "demo", "title": "The Name of the Wind"}',
            },
            {
                "name": "Terry Pratchett - Guards Guards",
                "protocol": "torrent",
                "media_kind": "book",
                "status": "pending",
                "progress": None,
                "metadata": '{"book_id": "demo-003", "provider": "demo", "title": "Guards! Guards!"}',
            },
            {
                "name": "Neil Gaiman - American Gods",
                "protocol": "torrent",
                "media_kind": "book",
                "status": "completed",
                "progress": 100.0,
                "metadata": '{"book_id": "demo-004", "provider": "demo", "title": "American Gods"}',
            },
        ])

    # Movie downloads
    if kobbler_movies:
        for i, movie in enumerate(kobbler_movies):
            status_info = statuses[i % len(statuses)]
            protocol = "torrent" if i % 2 == 0 else "usenet"
            metadata = json.dumps({
                "movie_id": movie["id"],
                "source_type": "movie",
                "title": movie["title"]
            })
            year_str = f" ({movie['year']})" if movie.get("year") else ""
            downloads.append({
                "name": f"{movie['title']}{year_str}",
                "protocol": protocol,
                "media_kind": "movie",
                "metadata": metadata,
                **status_info
            })
        print(f"  Using {len(kobbler_movies)} wanted movies from kobbler")

    # TV episode downloads
    if kobbler_episodes:
        for i, ep in enumerate(kobbler_episodes):
            status_info = statuses[i % len(statuses)]
            protocol = "torrent" if i % 2 == 0 else "usenet"
            metadata = json.dumps({
                "show_id": ep["show_id"],
                "source_type": "tv_episode",
                "season_id": ep["season_id"],
                "episode_map": {ep["episode_identifier"]: ep["episode_id"]},
            })
            downloads.append({
                "name": f"{ep['show_title']} S{int(ep['season_identifier']):02d}E{int(ep['episode_identifier']):02d}",
                "protocol": protocol,
                "media_kind": "tv",
                "metadata": metadata,
                **status_info
            })
        print(f"  Using {len(kobbler_episodes)} wanted TV episodes from kobbler")

    count = 0
    for i, dl in enumerate(downloads):
        dl_id = str(uuid.uuid4())
        progress_sql = str(dl["progress"]) if dl.get("progress") is not None else "NULL"
        error_sql = f"'{dl.get('error', '').replace(chr(39), chr(39)+chr(39))}'" if dl.get("error") else "NULL"
        media_kind = dl.get("media_kind")
        media_kind_sql = f"'{media_kind}'" if media_kind else "NULL"

        sql = f"""
        INSERT INTO downloads (
            id, name, source, protocol, download_url, callback_url,
            metadata, status, progress, error, api_key_id, media_kind,
            created_at, updated_at
        ) VALUES (
            '{dl_id}',
            '{dl["name"].replace(chr(39), chr(39)+chr(39))}',
            'prowlarr',
            '{dl["protocol"]}',
            'https://example.com/download/{dl_id}',
            'https://kobbler.local/api/v1/ingest/callback',
            '{dl["metadata"].replace(chr(39), chr(39)+chr(39))}'::jsonb,
            '{dl["status"]}',
            {progress_sql},
            {error_sql},
            '{api_key["id"]}',
            {media_kind_sql},
            NOW() - INTERVAL '{len(downloads) - i} hours',
            NOW() - INTERVAL '{max(0, len(downloads) - i - 1)} hours'
        ) ON CONFLICT DO NOTHING;
        """

        result = execute_sql(db_container, sql)
        if result is not None:
            count += 1

    return count


def main():
    parser = argparse.ArgumentParser(description="Seed demo data for Kobload")
    parser.add_argument("--db-container", required=True, help="Kobload Docker DB container name")
    parser.add_argument("--kobbler-db-container", default="kobbler-db",
                        help="Kobbler Docker DB container name (default: kobbler-db)")
    args = parser.parse_args()

    print("Seeding Kobload demo data...")

    # Create API key
    print("\nCreating API key...")
    api_key = seed_api_keys(args.db_container)
    if api_key:
        print(f"  API key ready: {api_key['name']}")
    else:
        print("  Failed to create API key")
        return

    # Verify media type configs exist (auto-seeded by migrations)
    print("\nChecking media type configs...")
    media_types = get_media_type_ids(args.db_container)
    print(f"  Found {len(media_types)} media types: {', '.join(media_types.keys())}")

    # Create quality profiles
    print("\nCreating quality profiles...")
    quality_profiles = seed_quality_profiles(args.db_container)
    print(f"  Created {len(quality_profiles)} quality profiles")

    # Try to fetch wanted items from kobbler
    print("\nLooking for kobbler wanted items...")
    kobbler_books = fetch_kobbler_wanted_books(args.kobbler_db_container)
    if kobbler_books:
        print(f"  Found {len(kobbler_books)} wanted books")
    else:
        print("  No wanted books found")

    kobbler_movies = fetch_kobbler_wanted_movies(args.kobbler_db_container)
    if kobbler_movies:
        print(f"  Found {len(kobbler_movies)} wanted movies")
    else:
        print("  No wanted movies found")

    kobbler_episodes = fetch_kobbler_wanted_episodes(args.kobbler_db_container)
    if kobbler_episodes:
        print(f"  Found {len(kobbler_episodes)} wanted TV episodes")
    else:
        print("  No wanted TV episodes found")

    # Create downloads
    print("\nCreating demo downloads...")
    download_count = seed_downloads(
        args.db_container, api_key,
        kobbler_books=kobbler_books,
        kobbler_movies=kobbler_movies,
        kobbler_episodes=kobbler_episodes,
    )
    print(f"  Created {download_count} downloads")

    # Print API key for configuration
    print("\n" + "="*60)
    print("Kobbler API Key:")
    print("="*60)
    print(f"  KOBLOAD_API_KEY={api_key['key']}")
    print("")
    print("This is a NAMED API key (stored in kobload's DB).")
    print("SSE streaming will filter to only show kobbler's downloads.")
    total_items = len(kobbler_books or []) + len(kobbler_movies or []) + len(kobbler_episodes or [])
    if total_items:
        print(f"\nDownloads linked to {total_items} wanted items from kobbler.")
    print("\nDone!")


if __name__ == "__main__":
    main()
