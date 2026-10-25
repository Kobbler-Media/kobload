/**
 * Shared status color definitions used across all pages.
 * Single source of truth — no more duplicated color maps.
 */

export type StatusStyle = { bg: string; color: string; label: string };

// --- Download status ---

const downloadStatusMap: Record<string, StatusStyle> = {
  pending: { bg: '#6b7280', color: '#fff', label: 'Pending' },
  queued: { bg: '#3b82f6', color: '#fff', label: 'Queued' },
  downloading: { bg: '#8b5cf6', color: '#fff', label: 'Downloading' },
  seeding: { bg: '#06b6d4', color: '#fff', label: 'Seeding' },
  processing: { bg: '#f59e0b', color: '#000', label: 'Processing' },
  completed: { bg: '#22c55e', color: '#fff', label: 'Completed' },
  failed: { bg: '#ef4444', color: '#fff', label: 'Failed' },
  cancelled: { bg: '#6b7280', color: '#fff', label: 'Cancelled' },
  imported: { bg: '#10b981', color: '#fff', label: 'Imported' }
};

const defaultDownloadStyle: StatusStyle = { bg: '#6b7280', color: '#fff', label: 'Unknown' };

export function getDownloadStatusStyle(status: string): StatusStyle {
  return downloadStatusMap[status] ?? { ...defaultDownloadStyle, label: status };
}

// --- Debrid stream attempt status ---

const streamAttemptStatusMap: Record<string, StatusStyle> = {
  resolving: { bg: '#3b82f6', color: '#fff', label: 'Resolving' },
  caching: { bg: '#8b5cf6', color: '#fff', label: 'Caching' },
  ready: { bg: '#22c55e', color: '#fff', label: 'Ready' },
  played: { bg: '#10b981', color: '#fff', label: 'Played' },
  uncached: { bg: '#6b7280', color: '#fff', label: 'Not Cached' },
  refused: { bg: '#f97316', color: '#fff', label: 'Refused' },
  failed: { bg: '#ef4444', color: '#fff', label: 'Failed' },
  abandoned: { bg: '#ca8a04', color: '#000', label: 'Abandoned' }
};

export const streamAttemptStatuses = Object.keys(streamAttemptStatusMap);

export function getStreamAttemptStatusStyle(status: string): StatusStyle {
  return streamAttemptStatusMap[status] ?? { ...defaultDownloadStyle, label: status };
}

// --- Watch pattern status (effective status after grouping logic) ---

const watchStatusMap: Record<string, { bg: string; color: string; label: string }> = {
  searching: { bg: 'rgba(59, 130, 246, 0.15)', color: '#3b82f6', label: 'Searching' },
  downloading: { bg: 'rgba(234, 179, 8, 0.15)', color: '#eab308', label: 'Downloading' },
  upgrading: { bg: 'rgba(34, 197, 94, 0.15)', color: '#22c55e', label: 'Upgrading' },
  satisfied: { bg: 'rgba(156, 163, 175, 0.15)', color: '#9ca3af', label: 'Satisfied' },
  paused: { bg: 'rgba(239, 68, 68, 0.15)', color: '#ef4444', label: 'Paused' },
  held: { bg: 'rgba(168, 85, 247, 0.15)', color: '#a855f7', label: 'Held' }
};

const defaultWatchStyle = { bg: 'rgba(156, 163, 175, 0.15)', color: '#9ca3af', label: 'Unknown' };

export function getWatchStatusStyle(status: string): typeof defaultWatchStyle {
  return watchStatusMap[status] ?? { ...defaultWatchStyle, label: status };
}

/** Solid-background variant for badges (dashboard, summaries) */
export function getWatchStatusSolid(status: string, enabled: boolean): StatusStyle {
  if (!enabled) return { bg: '#6b7280', color: '#fff', label: 'Disabled' };
  const solidMap: Record<string, StatusStyle> = {
    searching: { bg: '#3b82f6', color: '#fff', label: 'Searching' },
    found: { bg: '#22c55e', color: '#fff', label: 'Found' },
    downloading: { bg: '#8b5cf6', color: '#fff', label: 'Downloading' },
    completed: { bg: '#10b981', color: '#fff', label: 'Completed' }
  };
  return solidMap[status] ?? { bg: '#6b7280', color: '#fff', label: status };
}

// --- Watch match action ---

const matchActionMap: Record<string, { bg: string; color: string; label: string }> = {
  downloaded: { bg: '#22c55e', color: '#fff', label: 'Downloaded' },
  callback_sent: { bg: '#3b82f6', color: '#fff', label: 'Notified' },
  matched: { bg: '#3b82f6', color: '#fff', label: 'Matched' },
  held: { bg: '#a855f7', color: '#fff', label: 'Held' },
  skipped: { bg: '#eab308', color: '#000', label: 'Skipped' },
  upgrade: { bg: '#8b5cf6', color: '#fff', label: 'Upgrade' },
  replaced: { bg: '#6b7280', color: '#fff', label: 'Replaced' },
  rejected: { bg: '#ef4444', color: '#fff', label: 'Rejected' },
  error: { bg: '#ef4444', color: '#fff', label: 'Error' }
};

const defaultMatchAction = { bg: '#6b7280', color: '#fff', label: 'Unknown' };

export function getMatchActionStyle(action: string): typeof defaultMatchAction {
  return matchActionMap[action] ?? { ...defaultMatchAction, label: action };
}

// --- Score color ---

export function getScoreColor(score: number): string {
  if (score >= 0.9) return '#22c55e';
  if (score >= 0.75) return '#eab308';
  return '#ef4444';
}

// --- Sidebar detail status (flat color, used in detail sidebar) ---

const detailStatusColorMap: Record<string, string> = {
  searching: '#3b82f6',
  downloading: '#eab308',
  upgrading: '#22c55e',
  satisfied: '#9ca3af',
  paused: '#ef4444',
  held: '#a855f7'
};

export function getDetailStatusColor(status: string): string {
  return detailStatusColorMap[status] ?? '#9ca3af';
}

export function getDetailStatusLabel(status: string): string {
  return watchStatusMap[status]?.label ?? status;
}
