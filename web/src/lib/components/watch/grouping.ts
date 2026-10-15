import type { WatchPattern } from '$lib/types';

export type StatusCounts = {
  searching: number;
  downloading: number;
  upgrading: number;
  satisfied: number;
  paused: number;
  held: number;
};

export type PatternGroup = {
  key: string;
  label: string;
  mediaKind: string | null;
  patterns: WatchPattern[];
  statusCounts: StatusCounts;
};

export function getEffectiveStatus(p: WatchPattern): string {
  if (!p.enabled) return 'paused';
  if (p.episode_progress && p.episode_progress.imported < p.episode_progress.total) {
    return p.episode_progress.downloading > 0 ? 'downloading' : 'searching';
  }
  // upgrade_until is now the post-coverage cleanup timer, not the eligibility
  // gate. Pattern is "upgrading" whenever upgrade_allowed is true and something
  // has been imported; once the cleanup timer expires the pattern will be
  // removed entirely. Without an import (none yet, or every import rejected)
  // the server's searching/downloading/held status stands.
  const hasImport = p.status === 'upgrading' || p.status === 'satisfied';
  if (p.upgrade_allowed && hasImport) {
    if (p.upgrade_until && new Date(p.upgrade_until) <= new Date()) {
      return 'satisfied';
    }
    return 'upgrading';
  }
  return p.status;
}

export function getPatternTitle(pattern: WatchPattern): string {
  const mw = pattern.media_want;
  if (mw) {
    switch (mw.kind) {
      case 'tv_season':
        return `${mw.show_name} S${String(mw.season).padStart(2, '0')}`;
      case 'tv_episode':
        return `${mw.show_name} S${String(mw.season).padStart(2, '0')}E${String(mw.episode).padStart(2, '0')}`;
      case 'sport_event': {
        const parts = [mw.league_name];
        if (mw.location) parts.push(mw.location);
        if (mw.session_type) parts.push(mw.session_type);
        return parts.join(' – ');
      }
      case 'sport_season':
        return `${mw.league_name} ${mw.season}`;
      case 'music':
        return mw.album ? `${mw.artist} - ${mw.album}` : mw.artist;
      case 'software':
        return mw.version ? `${mw.name} ${mw.version}` : mw.name;
      default:
        if ('title' in mw && mw.title) return mw.title;
    }
  }
  const meta = pattern.metadata as Record<string, unknown> | null;
  return (meta?.title as string) || pattern.patterns[0] || 'Untitled';
}

/** Derive the search terms that would be used for a pattern's media_want.
 *  Mirrors the Rust `search_terms_from_media_want` logic so the UI can show
 *  what will actually be searched when `patterns` is empty. */
export function getSearchTermsFromMediaWant(pattern: WatchPattern): string[] {
  const mw = pattern.media_want;
  if (!mw) return [];
  const terms: string[] = [];

  switch (mw.kind) {
    case 'book': {
      if (mw.isbn) terms.push(mw.isbn);
      if (mw.author) {
        terms.push(`${mw.title} ${mw.author}`);
        terms.push(`${mw.author} ${mw.title}`);
      }
      if (mw.title) terms.push(mw.title);
      break;
    }
    case 'audiobook': {
      if ('isbn' in mw && (mw as Record<string, unknown>).isbn)
        terms.push(String((mw as Record<string, unknown>).isbn));
      if (mw.author) {
        terms.push(`${mw.title} ${mw.author}`);
        terms.push(`${mw.author} ${mw.title}`);
      }
      if (mw.narrator) terms.push(`${mw.title} ${mw.narrator}`);
      if (mw.title) terms.push(mw.title);
      break;
    }
    case 'tv_episode': {
      const s = String(mw.season).padStart(2, '0');
      const e = String(mw.episode).padStart(2, '0');
      terms.push(`${mw.show_name} S${s}E${e}`);
      if (mw.year) terms.push(`${mw.show_name} ${mw.year}`);
      break;
    }
    case 'tv_season': {
      const s = String(mw.season).padStart(2, '0');
      terms.push(`${mw.show_name} S${s}`);
      if (mw.year) terms.push(`${mw.show_name} ${mw.year}`);
      break;
    }
    case 'movie':
      if (mw.year) terms.push(`${mw.title} ${mw.year}`);
      terms.push(mw.title);
      break;
    case 'comic':
      if (mw.issue != null) terms.push(`${mw.title} ${mw.issue}`);
      terms.push(mw.title);
      break;
    case 'music':
      if (mw.album) terms.push(`${mw.artist} ${mw.album}`);
      terms.push(mw.artist);
      break;
    case 'game':
      terms.push(mw.title);
      break;
    case 'software':
      if (mw.version) terms.push(`${mw.name} ${mw.version}`);
      terms.push(mw.name);
      break;
    case 'sport_event':
      terms.push(mw.league_name);
      if (mw.event_title) terms.push(mw.event_title);
      break;
    case 'sport_season':
      terms.push(`${mw.league_name} ${mw.season}`);
      break;
  }

  return terms;
}

/** Derive the group key from a pattern's media_want and metadata */
function getGroupKey(pattern: WatchPattern): {
  key: string;
  label: string;
  mediaKind: string | null;
} {
  const mw = pattern.media_want;
  const meta = pattern.metadata as Record<string, unknown> | null;

  if (mw) {
    switch (mw.kind) {
      case 'tv_season':
      case 'tv_episode':
        return { key: `tv:${mw.show_name}`, label: mw.show_name, mediaKind: 'tv' };
      case 'sport_event':
        return {
          key: `sport:${mw.league_name}:${mw.season}`,
          label: `${mw.league_name} ${mw.season}`,
          mediaKind: 'sport'
        };
      case 'sport_season':
        return {
          key: `sport:${mw.league_name}:${mw.season}`,
          label: `${mw.league_name} ${mw.season}`,
          mediaKind: 'sport'
        };
      case 'book':
      case 'audiobook': {
        const series = meta?.series as string | undefined;
        if (series) return { key: `${mw.kind}:${series}`, label: series, mediaKind: mw.kind };
        return {
          key: `${mw.kind}:${getPatternTitle(pattern)}`,
          label: getPatternTitle(pattern),
          mediaKind: mw.kind
        };
      }
      case 'movie': {
        const series = meta?.series as string | undefined;
        if (series) return { key: `movie:${series}`, label: series, mediaKind: 'movie' };
        return { key: `movie:${mw.title}`, label: mw.title, mediaKind: 'movie' };
      }
      case 'comic': {
        const series = (meta?.series as string) || mw.series;
        if (series) return { key: `comic:${series}`, label: series, mediaKind: 'comic' };
        return { key: `comic:${mw.title}`, label: mw.title, mediaKind: 'comic' };
      }
      case 'music':
        return { key: `music:${mw.artist}`, label: mw.artist, mediaKind: 'music' };
      case 'game':
        return { key: `game:${mw.title}`, label: mw.title, mediaKind: 'game' };
      case 'software':
        return { key: `software:${mw.name}`, label: mw.name, mediaKind: 'software' };
    }
  }

  // Fallback: group by source_system
  return {
    key: `source:${pattern.source_system}:${getPatternTitle(pattern)}`,
    label: getPatternTitle(pattern),
    mediaKind: null
  };
}

function computeStatusCounts(patterns: WatchPattern[]): StatusCounts {
  const counts: StatusCounts = {
    searching: 0,
    downloading: 0,
    upgrading: 0,
    satisfied: 0,
    paused: 0,
    held: 0
  };
  for (const p of patterns) {
    const status = getEffectiveStatus(p);
    if (status === 'held') counts.held++;
    else if (status in counts) counts[status as keyof StatusCounts]++;
    else counts.searching++;
  }
  return counts;
}

/** Sort key for within a group -- by series_index, episode, season, then created_at */
function sortKeyWithinGroup(p: WatchPattern): number {
  const meta = p.metadata as Record<string, unknown> | null;
  const idx = meta?.series_index as number | undefined;
  if (idx != null) return idx;

  const mw = p.media_want;
  if (mw) {
    if ('episode' in mw && typeof mw.episode === 'number') {
      return (mw as { season: number }).season * 1000 + mw.episode;
    }
    if (mw.kind === 'sport_event' && mw.round != null) return mw.round;
    if ('season' in mw && typeof mw.season === 'number') return mw.season;
  }

  return new Date(p.created_at).getTime();
}

export function deriveGroups(patterns: WatchPattern[]): PatternGroup[] {
  const groupMap = new Map<
    string,
    { label: string; mediaKind: string | null; patterns: WatchPattern[] }
  >();

  for (const pattern of patterns) {
    const { key, label, mediaKind } = getGroupKey(pattern);
    const existing = groupMap.get(key);
    if (existing) {
      existing.patterns.push(pattern);
    } else {
      groupMap.set(key, { label, mediaKind, patterns: [pattern] });
    }
  }

  const groups: PatternGroup[] = [];
  for (const [key, group] of groupMap) {
    // Sort within group
    group.patterns.sort((a, b) => sortKeyWithinGroup(a) - sortKeyWithinGroup(b));

    groups.push({
      key,
      label: group.label,
      mediaKind: group.mediaKind,
      patterns: group.patterns,
      statusCounts: computeStatusCounts(group.patterns)
    });
  }

  // Sort groups by most recent pattern created_at
  groups.sort((a, b) => {
    const aLatest = Math.max(...a.patterns.map((p) => new Date(p.created_at).getTime()));
    const bLatest = Math.max(...b.patterns.map((p) => new Date(p.created_at).getTime()));
    return bLatest - aLatest;
  });

  return groups;
}
