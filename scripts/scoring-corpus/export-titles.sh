#!/usr/bin/env bash
# Export real release titles for parser reference tests (has_word, Aho-Corasick,
# TRaSH RegexSet swaps): titles that matched a want, plus a random sample of
# DHT names, which carry the junk feeds actually serve (foreign, anime,
# obfuscated, scene variants). Read-only against the database.
#
# Usage: scripts/scoring-corpus/export-titles.sh [out_dir] [sample_size]
#   out_dir      default data/scoring-corpus (gitignored)
#   sample_size  DHT names to sample, default 50000 (~1.5 MB gzipped)
#
# Env: DB_CONTAINER (default kobload-db-1), DB_USER/DB_NAME (default kobload).
set -euo pipefail

out_dir=${1:-data/scoring-corpus}
sample=${2:-50000}
container=${DB_CONTAINER:-kobload-db-1}
user=${DB_USER:-kobload}
db=${DB_NAME:-kobload}

[[ $sample =~ ^[0-9]+$ ]] || { echo "sample_size must be a number" >&2; exit 1; }
mkdir -p "$out_dir"

psql() {
    docker exec -i "$container" psql -U "$user" -d "$db" -v ON_ERROR_STOP=1 -AtX "$@"
}

# Matched titles: known positives, small.
psql -c "SELECT DISTINCT release_title FROM watch_matches
         WHERE release_title IS NOT NULL AND release_title <> ''" \
    | gzip -9 > "$out_dir/titles-matched.txt.gz"

# DHT sample. TABLESAMPLE keeps this cheap on an 11M-row table; the percentage
# is sized from the estimated row count so it returns roughly $sample rows.
rows=$(psql -c "SELECT GREATEST(reltuples::bigint, 1) FROM pg_class WHERE relname = 'torrent_hash_index'")
pct=$(awk -v s="$sample" -v r="$rows" 'BEGIN { p = s * 150 / r; if (p > 100) p = 100; printf "%.6f", p }')
psql -c "SELECT original_title FROM torrent_hash_index TABLESAMPLE SYSTEM ($pct)
         WHERE original_title IS NOT NULL AND original_title <> ''
         LIMIT $sample" \
    | gzip -9 > "$out_dir/titles-dht-sample.txt.gz"

for f in titles-matched titles-dht-sample; do
    echo "$out_dir/$f.txt.gz: $(zcat "$out_dir/$f.txt.gz" | wc -l) titles, $(du -h "$out_dir/$f.txt.gz" | cut -f1)"
done
