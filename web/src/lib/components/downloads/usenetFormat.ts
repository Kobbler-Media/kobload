import type {
  UsenetEngineActivity,
  UsenetJobPhase,
  UsenetStreamKind,
  UsenetWaitReason
} from '#lib/types.js';

export const phaseLabels: Record<UsenetJobPhase, string> = {
  queued: 'Queued',
  downloading: 'Downloading',
  verifying: 'Verifying',
  repairing: 'Repairing',
  unpacking: 'Unpacking'
};

export const waitLabels: Record<UsenetWaitReason, string> = {
  stream: 'Waiting for a stream to finish',
  low_disk: 'Paused: low disk space'
};

export const streamLabels: Record<UsenetStreamKind, string> = {
  playing: 'Playing',
  caching_ahead: 'Caching ahead',
  preloading: 'Preloading'
};

export const activityLabel = (activity: UsenetEngineActivity): string => {
  switch (activity.kind) {
    case 'idle':
      return 'Idle';
    case 'downloading':
      return 'Downloading';
    case 'streaming':
      return 'Streaming';
    case 'downloads_waiting_for_stream':
      return 'Downloads waiting for a stream';
    case 'paused_low_disk':
      return 'Paused: low disk space';
    case 'provider_failing':
      return `Provider failing: ${activity.host}`;
  }
};

export const formatSpeed = (bps: number | null): string =>
  bps === null ? '–' : `${(bps / (1024 * 1024)).toFixed(1)} MB/s`;

export const formatEta = (seconds: number): string => {
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  if (hours > 0) return `${hours}h ${minutes}m`;
  if (minutes > 0) return `${minutes}m ${seconds % 60}s`;
  return `${seconds}s`;
};
