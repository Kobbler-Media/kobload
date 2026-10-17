#!/usr/bin/env python3
"""Build a supplementary scoring corpus of real 2026 motorsport releases.

A one-week RSS capture only sees the sport events that happen that week, but
most sport wants are for past rounds. This replays sport releases Kobload has
already seen against a captured pattern set, so the session, round and
location gates get real positive and negative cases:

  * releases grabbed for sport wants (downloads.release_payload)
  * releases matched to sport wants (watch_matches.release_data)
  * 2026 F1/MotoGP/Moto2/Moto3/SBK titles from the DHT hash index

Output is a capture directory that `read_capture` and the replay tests accept
(KOBLOAD_SCORING_CORPUS=<out>). Releases get a "supp-" GUID prefix so the
pattern set's matched_guids doesn't skip them, and are grouped into one fetch
per origin and ISO week, with the capture time pinned an hour after the
newest release in the group.

Read-only against the database. The output contains the watchlist, so it
belongs under the gitignored data/ directory.

  scripts/scoring-corpus/build-sport-supplement.py \\
      --capture data/scoring-capture --out data/scoring-capture-sport
"""

import argparse
import gzip
import json
import re
import shutil
import subprocess
import sys
from collections import defaultdict
from datetime import datetime, timedelta, timezone
from email.utils import format_datetime, parsedate_to_datetime
from pathlib import Path

SPORT_RE = r"(motogp|moto ?[23]\M|wsbk|superbike|formula.?1|\mf1\M)"
DB = ["docker", "exec", "-i", "kobload-db-1", "psql", "-U", "kobload", "-d", "kobload", "-AtX"]


def query(sql):
    out = subprocess.run(DB, input=sql, capture_output=True, text=True, check=True).stdout
    return [json.loads(line) for line in out.splitlines() if line.strip()]


def pick(d, *keys):
    for k in keys:
        if d.get(k) is not None:
            return d[k]
    return None


def parse_date(s):
    if not s:
        return None
    try:
        return parsedate_to_datetime(s).astimezone(timezone.utc)
    except (TypeError, ValueError):
        pass
    try:
        return datetime.fromisoformat(s.replace("Z", "+00:00")).astimezone(timezone.utc)
    except ValueError:
        return None


def category_ids(d):
    ids = d.get("category_ids")
    if isinstance(ids, list) and ids:
        return [int(i) for i in ids]
    out = []
    for c in d.get("categories") or []:
        if isinstance(c, dict):
            if c.get("id") is not None:
                out.append(int(c["id"]))
            out += [int(s["id"]) for s in c.get("subCategories") or [] if s.get("id") is not None]
        elif isinstance(c, int):
            out.append(c)
    return out or [5000, 5060]


def as_int(v):
    try:
        return int(v) if v is not None else None
    except (TypeError, ValueError):
        return None


def strip_query(url):
    # Stored indexer URLs carry API keys; scoring only needs to know a URL exists.
    # Newznab download links often skip the "?" ("...nzb&i=1&r=<key>").
    return re.split(r"[?&]", url, maxsplit=1)[0] if isinstance(url, str) else url


def release_from_payload(d, fallback_date):
    """Map a stored release (Prowlarr camelCase or Kobload snake_case) to `Release`."""
    published = parse_date(pick(d, "publish_date", "publishDate")) or fallback_date
    r = {
        "guid": "supp-" + str(pick(d, "guid", "id") or pick(d, "title")),
        "title": d["title"],
        "file_name": pick(d, "file_name", "fileName"),
        "download_url": strip_query(pick(d, "download_url", "downloadUrl")),
        "magnet_url": pick(d, "magnet_url", "magnetUrl"),
        "info_url": strip_query(pick(d, "info_url", "infoUrl")),
        "size": as_int(d.get("size")) or 0,
        "publish_date": format_datetime(published),
        "category_ids": category_ids(d),
        "indexer": str(pick(d, "indexer") or "unknown"),
        "protocol": pick(d, "protocol") or "usenet",
        "raw_data": {},
    }
    optional = {
        "info_hash": pick(d, "info_hash", "infoHash"),
        "files": as_int(d.get("files")),
        "seeders": as_int(d.get("seeders")),
        "peers": as_int(d.get("peers")),
        "leechers": as_int(d.get("leechers")),
        "grabs": as_int(d.get("grabs")),
        "votes": as_int(d.get("votes")),
        "indexer_source_id": pick(d, "indexer_source_id"),
    }
    r.update({k: v for k, v in optional.items() if v is not None})
    return published, r


def release_from_dht(row):
    published = parse_date(row["first_seen"])
    r = {
        "guid": "supp-dht-" + row["info_hash"],
        "title": row["original_title"],
        "file_name": None,
        "download_url": None,
        "magnet_url": row.get("magnet_url") or f"magnet:?xt=urn:btih:{row['info_hash']}",
        "info_url": None,
        "size": row.get("size") or 0,
        "publish_date": format_datetime(published),
        "category_ids": [5000, 5060],
        "indexer": "DHT",
        "protocol": "torrent",
        "info_hash": row["info_hash"],
        "raw_data": {},
    }
    if row.get("seeders") is not None:
        r["seeders"] = row["seeders"]
    return published, r


def latest_fetch_for(capture, handler_types):
    """Newest captured fetch from a source of the given kind: its pattern set,
    TRaSH rows and source identity are reused for the supplementary fetches."""
    best = None
    with gzip.open(capture / "fetches.jsonl.gz", "rt") as f:
        for line in f:
            fetch = json.loads(line)
            if fetch["handler_type"] in handler_types:
                best = fetch
    if best is None:
        sys.exit(f"no {handler_types} fetch in {capture}")
    return best


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--capture", type=Path, default=Path("data/scoring-capture"))
    ap.add_argument("--out", type=Path, default=Path("data/scoring-capture-sport"))
    args = ap.parse_args()

    if (args.out / "fetches.jsonl.gz").exists():
        sys.exit(f"{args.out} already has a corpus; remove it first")

    usenet = latest_fetch_for(args.capture, {"newznab"})
    torrent = latest_fetch_for(args.capture, {"torrentday", "mam"})

    grabbed = query(f"""
        SELECT json_build_object('created_at', created_at, 'payload', release_payload)
        FROM downloads
        WHERE release_payload IS NOT NULL
          AND (media_kind = 'sports' OR name ~* '{SPORT_RE}')
    """)
    matched = query("""
        SELECT json_build_object('created_at', m.created_at, 'payload', m.release_data)
        FROM watch_matches m JOIN watch_patterns w ON w.id = m.watch_pattern_id
        WHERE w.media_want->>'kind' = 'sport_event'
    """)
    dht = query(f"""
        SELECT row_to_json(t) FROM (
            SELECT info_hash, original_title, size, seeders, magnet_url, first_seen
            FROM torrent_hash_index
            WHERE original_title ~* '{SPORT_RE}' AND original_title ~ '2026'
        ) t
    """)

    groups = defaultdict(dict)  # (origin, week) -> guid -> (published, release)
    seen = set()

    def add(origin, published, release):
        # A grabbed release usually also has a watch_matches row.
        if release["guid"] in seen:
            return
        seen.add(release["guid"])
        week = published.isocalendar()[:2]
        groups[(origin, week)][release["guid"]] = (published, release)

    for row in grabbed + matched:
        published, r = release_from_payload(row["payload"], parse_date(row["created_at"]))
        add("usenet" if r["protocol"] == "usenet" else "torrent", published, r)
    for row in dht:
        published, r = release_from_dht(row)
        add("dht", published, r)

    args.out.mkdir(parents=True)
    for kind in ("patterns", "trash"):
        (args.out / kind).mkdir()
    for fetch in (usenet, torrent):
        shutil.copy(args.capture / "patterns" / f"{fetch['patterns']}.json.gz", args.out / "patterns")
        if fetch.get("trash"):
            shutil.copy(args.capture / "trash" / f"{fetch['trash']}.json.gz", args.out / "trash")

    fetches = []
    for (origin, _week), items in groups.items():
        base = usenet if origin == "usenet" else torrent
        releases = [r for _, r in sorted(items.values(), key=lambda pr: pr[0])]
        newest = max(p for p, _ in items.values())
        fetches.append({
            "captured_at": (newest + timedelta(hours=1)).isoformat().replace("+00:00", "Z"),
            "source_id": base["source_id"],
            "source_name": f"supplement:{origin}",
            "handler_type": base["handler_type"],
            "patterns": base["patterns"],
            "trash": base.get("trash"),
            "releases": releases,
        })
    fetches.sort(key=lambda f: (f["captured_at"], f["source_name"]))

    with open(args.out / "fetches.jsonl.gz", "wb") as out:
        for fetch in fetches:
            out.write(gzip.compress((json.dumps(fetch) + "\n").encode()))
    started = min(f["captured_at"] for f in fetches)
    until = max(f["captured_at"] for f in fetches)
    (args.out / "capture.json").write_text(json.dumps(
        {"version": 1, "started_at": started, "until": until, "max_bytes": 0}, indent=2))

    total = sum(len(f["releases"]) for f in fetches)
    print(f"{len(fetches)} fetches, {total} releases "
          f"(grabbed {len(grabbed)}, matched {len(matched)}, dht {len(dht)}) -> {args.out}")


if __name__ == "__main__":
    main()
