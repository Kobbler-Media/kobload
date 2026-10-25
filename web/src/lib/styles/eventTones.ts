export type EventTone = 'good' | 'info' | 'accent' | 'bad' | 'warn';

export type TimelineEvent = {
  id: string;
  event_type: string;
  message: string | null;
  details?: Record<string, unknown> | null;
  created_at: string;
};

export const downloadEventTones: Record<string, EventTone> = {
  created: 'good',
  client_add_success: 'good',
  status_changed: 'info',
  callback_sent: 'accent',
  import_confirmed: 'accent',
  reingest_started: 'accent',
  client_add_failed: 'bad',
  poll_error: 'bad',
  callback_failed: 'bad',
  remove_failed: 'bad',
  cancelled: 'warn',
  removed: 'warn'
};

export const streamEventTones: Record<string, EventTone> = {
  ready: 'good',
  played: 'good',
  warm_hit: 'good',
  requested: 'info',
  added: 'info',
  reused: 'info',
  readded: 'info',
  selected: 'info',
  cache_check: 'info',
  joined: 'info',
  caching: 'accent',
  failed: 'bad',
  refused: 'bad',
  known_blocked: 'bad',
  playback_failed: 'bad',
  timed_out: 'bad',
  released: 'warn',
  poll_error: 'warn',
  abandoned: 'warn'
};
