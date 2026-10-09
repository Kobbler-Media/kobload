# Hash Index Seed Data

Pre-processed torrent metadata for seeding the `torrent_hash_index` table.

## Input layout

Bring-your-own: nothing here ships with the repo. `seed-processor` looks for each
optional source at a fixed path under `--seeds-dir` and skips any that are absent.
The directory names follow the historical schema each reader expects (see the
matching `process_*` function in `api/src/bin/seed_processor.rs` for the exact
columns read).

```
data/seeds/
  rarbg/rarbg_db.sqlite           # SQLite, RARBG-style schema        (process_rarbg)
  magnetdb/magnetdb_public.sqlite3 # SQLite, MagnetDB-style schema    (process_magnetdb)
  magnetdb/matchedVideos.csv      # optional hash → IMDB mapping CSV
  piratebay/*.csv                 # CSV, base64 info hashes           (process_tpb)
  rutracker/<snapshot>/           # category CSVs                     (process_rutracker)
  dmm-hashlists/                  # DMM hashlist HTML files
  imdb/title.basics.tsv.gz        # IMDB title dump for offline enrichment
  processed/hash_index.tsv        # OUTPUT: COPY-ready TSV for PostgreSQL
```

### DMM Hashlists (refreshable, actively maintained)
```bash
cd data/seeds
git clone --depth 1 https://github.com/debridmediamanager/hashlists.git dmm-hashlists
# To refresh: cd dmm-hashlists && git pull
```

### IMDB Title Dump (refreshable, updated daily)
```bash
mkdir -p data/seeds/imdb
cd data/seeds/imdb
wget https://datasets.imdbws.com/title.basics.tsv.gz
# ~209MB, no login required, non-commercial use
# Refresh periodically for new titles
```

## Pre-Processing

The seed processor reads all sources, filters to video/TV/books/sports content,
enriches IMDB IDs from multiple sources, deduplicates by info_hash, and writes
a PostgreSQL COPY-ready TSV file.

```bash
# Build (one time)
cargo build --release --bin seed-processor

# Run
./target/release/seed-processor \
  --seeds-dir data/seeds \
  --output data/seeds/processed/hash_index.tsv
```

The processor:
1. Loads the IMDB title dump into memory (title+year → IMDB mappings)
2. Loads the optional hash → IMDB mapping CSV
3. Processes each present source in order: RARBG-style → MagnetDB-style → TPB-style → RuTracker-style → DMM
4. Filters: keeps movies, TV, audiobooks, ebooks, sports. Rejects games, software, music, XXX.
5. Enriches IMDB IDs from: source data > matchedVideos > IMDB title+year match
6. Deduplicates by info_hash (first source wins, later sources enrich nulls)
7. Writes tab-separated output with PostgreSQL NULL markers (`\N`)

**Note:** The output may contain null bytes or non-UTF8 from some CSV inputs. Clean with:
```bash
python3 -c "
with open('data/seeds/processed/hash_index.tsv', 'rb') as f:
    with open('data/seeds/processed/hash_index_clean.tsv', 'wb') as out:
        for line in f:
            cleaned = line.replace(b'\x00', b'')
            text = cleaned.decode('utf-8', errors='ignore')
            out.write(text.encode('utf-8'))
"
```

## Loading into PostgreSQL

### Option A: Upload via UI
Gzip the clean TSV and upload via Settings → Debrid → Hash Index → Bootstrap:
```bash
gzip -k data/seeds/processed/hash_index_clean.tsv
# Upload hash_index_clean.tsv.gz via the web UI
```

### Option B: Direct COPY (faster, requires DB access)
```bash
# Copy file into the database container
docker cp data/seeds/processed/hash_index_clean.tsv kobload-db:/tmp/hash_load.tsv

# Load via psql
docker exec kobload-db psql -U kobload -d kobload -c "
DROP INDEX IF EXISTS idx_hash_index_imdb;
DROP INDEX IF EXISTS idx_hash_index_tmdb;
DROP INDEX IF EXISTS idx_hash_index_title;
DROP INDEX IF EXISTS idx_hash_index_source;
ALTER TABLE torrent_hash_index SET (autovacuum_enabled = false);
TRUNCATE torrent_hash_index;
COPY torrent_hash_index(info_hash, imdb_id, tmdb_id, original_title, parsed_title, year, quality, size, magnet_url, source, seeders, season, episodes) FROM '/tmp/hash_load.tsv' WITH (FORMAT text, HEADER true, NULL '\N');
CREATE INDEX idx_hash_index_imdb ON torrent_hash_index(imdb_id) WHERE imdb_id IS NOT NULL;
CREATE INDEX idx_hash_index_tmdb ON torrent_hash_index(tmdb_id) WHERE tmdb_id IS NOT NULL;
CREATE INDEX idx_hash_index_title ON torrent_hash_index USING gin(to_tsvector('english', original_title));
CREATE INDEX idx_hash_index_source ON torrent_hash_index(source);
ALTER TABLE torrent_hash_index SET (autovacuum_enabled = true);
ANALYZE torrent_hash_index;
"
```

## Refreshing

Only DMM hashlists and IMDB title dump are worth refreshing:

```bash
# DMM (monthly)
cd data/seeds/dmm-hashlists && git pull

# IMDB (when you want newer title mappings)
cd data/seeds/imdb && wget -O title.basics.tsv.gz https://datasets.imdbws.com/title.basics.tsv.gz

# Then re-run the processor and reload
```

BitMagnet handles continuous DHT discovery separately via the scheduled sync job.
