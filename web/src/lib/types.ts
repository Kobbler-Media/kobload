// Shared TypeScript types for Kobload web UI

export type Book = {
  id: string;
  title: string;
  authors: string[];
  description: string | null;
  publisher: string | null;
  pubdate: string | null;
  series_id: string | null;
  series_name: string | null;
  series_index: number | null;
  series_index_float: number | null;
  language: string | null;
  formats: string[];
  synced: boolean;
  pending_sync: boolean;
  cover_url: string;
  created_at: string;
  updated_at: string;
};

export type UserSummary = {
  id: string;
  email: string;
};

export type DeviceSummary = {
  id: string;
  name: string | null;
  user_id: string;
  token: string;
  sync_url: string;
  kobo_api_url: string | null;
  last_sync_at: string | null;
};

export type ActivityEvent = {
  id: string;
  source: string;
  status: string;
  message: string | null;
  created_at: string;
  book_id: string | null;
  title: string | null;
  cover_url: string | null;
  device_id: string | null;
  device_name: string | null;
};

export type SeriesSummary = {
  id: string;
  name: string;
  book_count: number;
  authors: string[];
  updated_at: string;
  cover_urls: string[];
};

export type ShelfSummary = {
  id: string;
  name: string;
  user_id: string;
  book_count: number;
  last_modified: string;
  cover_urls: string[];
};

export type AuthorSummary = {
  id: string;
  name: string;
  book_count: number;
  cover_urls: string[];
};

export type IngestTokenSummary = {
  id: string;
  token: string;
  created_at: string;
  revoked: boolean;
};

export type WatchDirectory = {
  id: string;
  path: string;
  name: string;
  enabled: boolean;
  recursive: boolean;
  media_type: 'book' | 'tv' | 'movie';
  callback_url: string;
  auth_token: string | null;
  scan_status: 'idle' | 'scanning' | 'error';
  scan_error: string | null;
  error_count: number;
  next_retry_at: string | null;
  last_scan_at: string | null;
  created_at: string;
  updated_at: string;
};

export type WatchDirectoryWithStats = WatchDirectory & {
  file_counts: { status: string; count: number }[];
};

export type WatchDirectoryFile = {
  id: string;
  watch_directory_id: string;
  file_path: string;
  file_name: string;
  file_size: number;
  file_hash: string | null;
  file_mtime: string | null;
  interpretation: Record<string, unknown> | null;
  status: 'discovered' | 'sent' | 'skipped' | 'error';
  download_id: string | null;
  callback_sent_at: string | null;
  callback_error: string | null;
  created_at: string;
  updated_at: string;
};

export type DirectoryEntry = {
  name: string;
  path: string;
};

export type WantedItem = {
  id: string;
  kind: string;
  provider: string;
  provider_id: string | null;
  title: string;
  author: string | null;
  series: string | null;
  series_index: number | null;
  isbn10: string | null;
  isbn13: string | null;
  status: string;
  last_attempt_at: string | null;
  created_at: string;
  updated_at: string;
};

export type WantedAttempt = {
  id: string;
  wanted_id: string;
  source: string;
  external_id: string | null;
  release_payload: Record<string, unknown> | null;
  download_client_id: number | null;
  download_url: string | null;
  status: string;
  message: string | null;
  created_at: string;
};

export type WantedDetail = {
  item: WantedItem;
  attempts: WantedAttempt[];
};

export type ProwlarrRelease = {
  guid?: string;
  indexerId?: number;
  title?: string;
  size?: number;
  indexer?: string;
  seeders?: number;
  protocol?: string;
  publishDate?: string;
  downloadUrl?: string;
  [key: string]: unknown;
};

export type ProwlarrDownloadClient = {
  id: number;
  name?: string | null;
  implementation?: string | null;
  enable: boolean;
  protocol?: string | null;
};

export type UnavailableBook = {
  id: string;
  title: string;
  authors: string[];
  unavailable_files: number;
  total_files: number;
  unavailable_since: string | null;
  cover_url: string;
};

export type UnavailableBooksResponse = {
  items: UnavailableBook[];
  total_count: number;
};

export type IngestQueueStatusCounts = {
  extracting: number;
  pending: number;
  converting: number;
  approved: number;
  rejected: number;
  error: number;
};

export type IngestQueueItem = {
  id: string;
  watch_directory_id: string | null;
  file_path: string;
  file_name: string;
  file_size: number;
  file_hash: string | null;
  source_mtime: string | null;
  source_format: string;
  extracted_title: string | null;
  extracted_authors: string[] | null;
  extracted_isbn: string | null;
  extracted_description: string | null;
  extracted_series: string | null;
  extracted_series_index: number | null;
  extracted_language: string | null;
  extracted_publisher: string | null;
  extracted_pubdate: string | null;
  has_cover: boolean;
  selected_metadata: Record<string, unknown> | null;
  status: 'extracting' | 'pending' | 'converting' | 'approved' | 'rejected' | 'error';
  conversion_error: string | null;
  book_id: string | null;
  update_target_book_id: string | null;
  created_at: string;
};

/** A group of ingest queue items with the same base filename (different formats) */
export type IngestQueueGroup = {
  /** Unique key for this group (directory|basename) */
  group_key: string;
  /** All items in this group (different format files) */
  items: IngestQueueItem[];
  /** Available formats in this group (e.g., ["EPUB", "MOBI"]) */
  formats: string[];
  /** ID of the primary (preferred) item */
  primary_item_id: string;
  /** Computed display status for the group */
  display_status: IngestQueueItem['status'];
};

export type IngestQueueListResponse = {
  groups: IngestQueueGroup[];
  total_groups: number;
  status_counts: IngestQueueStatusCounts;
};

export type MetadataProvider = {
  id: string;
  provider_type: string;
  enabled: boolean;
  priority: number;
  api_key: string | null;
  rate_limit_per_minute: number;
  created_at: string;
  updated_at: string;
};

export type Job = {
  id: string;
  job_type: string;
  status: 'pending' | 'running' | 'completed' | 'failed';
  payload: Record<string, unknown>;
  result: Record<string, unknown> | null;
  error_message: string | null;
  progress: number | null;
  created_at: string;
  started_at: string | null;
  completed_at: string | null;
};

export type JobEvent = {
  job_id: string;
  job_type: string;
  status: string;
  progress: number | null;
  error: string | null;
};

// Pagination types
export type PaginatedResponse<T> = {
  items: T[];
  next_cursor: string | null;
};

// UI state types
export type PageKey = 'downloads' | 'settings';
export type LoadStatus = 'idle' | 'loading' | 'error' | 'success';
export type LoadKey = 'downloads' | 'settings';
export type LoadState = { status: LoadStatus; error: string };

// Callback types for component props
export type LoginHandler = (email: string, password: string) => Promise<void>;
export type NavigateHandler = (page: PageKey) => void;
export type BookSelectHandler = (bookId: string, event?: MouseEvent | KeyboardEvent) => void;
export type SaveEditsHandler = (bookId: string, payload: Record<string, unknown>) => Promise<void>;
export type ShelfActionHandler = (shelfId: string) => void;
export type CreateShelfHandler = (name: string) => Promise<string | null>;
export type SearchInputHandler = (value: string) => void;
export type SendToDeviceHandler = (deviceIds: string[], bookIds: string[]) => Promise<void>;

// Watch pattern types (Kobload-specific)
export type EpisodeProgress = {
  total: number;
  imported: number;
  downloading: number;
};

export type WatchPattern = {
  id: string;
  source_system: string;
  source_id: string;
  callback_url: string;
  patterns: string[];
  media_want?: MediaWant;
  match_mode: string;
  min_score: number | null;
  media_type: string | null;
  auto_download: boolean;
  metadata: Record<string, unknown> | null;
  enabled: boolean;
  status: string;
  upgrade_allowed: boolean;
  upgrade_until: string | null;
  best_quality_score: number | null;
  episode_progress?: EpisodeProgress | null;
  created_at: string;
  updated_at: string;
};

export type WatchPatternDetail = {
  pattern: WatchPattern;
  matches: WatchMatch[];
};

export type IndexerSource = {
  id: string;
  name: string;
  handler_type: string;
  handler_config: Record<string, unknown>;
  category_ids: number[] | null;
  enabled: boolean;
  last_check_at: string | null;
  last_error: string | null;
  state: Record<string, unknown> | null;
  priority: number;
  last_success_at: string | null;
  last_failure_at: string | null;
  consecutive_failures: number;
  health_status: string | null;
  auth_failed_at: string | null;
  created_at: string;
  seed_ratio: number | null;
  seed_time_minutes: number | null;
  capabilities: Record<string, unknown> | null;
  caps_fetched_at: string | null;
  api_limit_daily: number | null;
  api_calls_today: number;
  search_mode: string;
  auto_throttle: boolean;
  debrid_mode: string;
};

export type IndexerCategoryMapping = {
  id: string;
  indexer_source_id: string;
  category_key: string;
  indexer_category_id: number;
  created_at: string;
};

export type IndexerRssCacheInfo = {
  indexer_source_id: string;
  fetched_at: string;
  ttl_seconds: number;
  expires_at: string;
  size_bytes: number;
};

export type WatchMatch = {
  id: string;
  watch_pattern_id: string | null;
  indexer_source_id: string | null;
  release_guid: string;
  release_title: string;
  release_data: Record<string, unknown>;
  match_score: number;
  matched_pattern: string | null;
  action: string;
  download_id: string | null;
  callback_sent_at: string | null;
  callback_error: string | null;
  quality_score: number | null;
  rejected: boolean;
  rejected_at: string | null;
  rejected_reason: string | null;
  episode_key: string | null;
  created_at: string;
};

export type ProcessedRelease = {
  guid: string;
  title: string;
  file_name: string | null;
  size: number;
  indexer: string;
  protocol: string;
  download_url: string | null;
  magnet_url: string | null;
  info_url: string | null;
  publish_date: string | null;
  indexer_source_id?: string | null;
  decision: 'candidate' | 'rejected' | 'no_match' | 'excluded';
  reject_reasons?: Array<{ category: string; reason?: string }>;
  match_score?: number | null;
  matched_pattern?: string | null;
  matched_target?: Record<string, unknown> | null;
  quality_score?: number | null;
  total_score?: number | null;
  quality?: Record<string, unknown> | null;
  quality_rejected: boolean;
  quality_reject_reasons?: string[];
  structural_rejected: boolean;
  structural_reject_reason?: string | null;
  episode_key?: string | null;
  context_flags?: string[];
};

export type SearchAndDownloadResponse = {
  download_id: string | null;
  selected_release: ProcessedRelease | null;
  results: ProcessedRelease[];
};

export type QualityProfile = {
  id: string;
  name: string;
  media_kind: string;
  min_resolution: string | null;
  max_resolution: string | null;
  preferred_sources: string[];
  preferred_codecs: string[];
  preferred_audio: string[];
  preferred_languages: string[];
  rejected_sources: string[];
  rejected_languages: string[];
  rejected_terms: string[];
  reject_hardcoded_subs: boolean;
  use_trash_filters: boolean;
  reject_remux: boolean;
  upgrade_cutoff_score: number | null;
  is_default: boolean;
  created_at: string;
  updated_at: string;
};

export type MediaWant =
  | {
      kind: 'book';
      title: string;
      author?: string | null;
      isbn?: string | null;
      format_preferences?: string[];
      identifiers?: Record<string, unknown> | null;
    }
  | {
      kind: 'audiobook';
      title: string;
      author?: string | null;
      narrator?: string | null;
    }
  | {
      kind: 'tv_episode';
      show_name: string;
      season: number;
      episode: number;
      year?: number | null;
      identifiers?: Record<string, unknown> | null;
      quality_profile_id?: string | null;
      quality_overrides?: Record<string, unknown> | null;
    }
  | {
      kind: 'tv_season';
      show_name: string;
      season: number;
      year?: number | null;
      identifiers?: Record<string, unknown> | null;
      quality_profile_id?: string | null;
      quality_overrides?: Record<string, unknown> | null;
    }
  | {
      kind: 'movie';
      title: string;
      year?: number | null;
      identifiers?: Record<string, unknown> | null;
      quality_profile_id?: string | null;
      quality_overrides?: Record<string, unknown> | null;
    }
  | {
      kind: 'comic';
      title: string;
      issue?: number | null;
      series?: string | null;
      year?: number | null;
    }
  | {
      kind: 'music';
      artist: string;
      album?: string | null;
      year?: number | null;
    }
  | {
      kind: 'game';
      title: string;
      platform?: string | null;
      year?: number | null;
    }
  | {
      kind: 'software';
      name: string;
      version?: string | null;
      platform?: string | null;
    }
  | {
      kind: 'sport_event';
      league_name: string;
      season: string;
      round?: number | null;
      location?: string | null;
      session_type?: string | null;
      event_title?: string | null;
      quality_profile_id?: string | null;
      quality_overrides?: Record<string, unknown> | null;
    }
  | {
      kind: 'sport_season';
      league_name: string;
      season: string;
      quality_profile_id?: string | null;
      quality_overrides?: Record<string, unknown> | null;
    };

// Decision Log types

export type DecisionLogSession = {
  id: string;
  trigger_type: string;
  watch_pattern_id: string | null;
  indexer_source_id: string | null;
  is_test: boolean;
  outcome: string | null;
  download_id: string | null;
  releases_evaluated: number;
  search_queries: string[] | null;
  error_message: string | null;
  started_at: string;
  finished_at: string | null;
  duration_ms: number | null;
  created_at: string;
  pattern_name: string | null;
  source_name: string | null;
  pattern_metadata: Record<string, unknown> | null;
};

export type DecisionLogProviderCall = {
  id: string;
  indexer_source_id: string | null;
  source_name: string;
  handler_type: string;
  search_type: string | null;
  query: string | null;
  categories: unknown | null;
  external_ids: unknown | null;
  results_count: number;
  results_after_dedup: number;
  status: string;
  error_message: string | null;
  duration_ms: number | null;
};

export type QualityScoreBreakdown = {
  resolution_tier?: number;
  min_resolution_check?: number;
  max_resolution_check?: number;
  preferred_source?: number;
  rejected_source?: number;
  preferred_codec?: number;
  preferred_audio?: number;
  hardcoded_subs?: number;
  remux_rejection?: number;
  proper_repack?: number;
  real?: number;
  remux_bonus?: number;
  dolby_vision?: number;
  hdr10plus?: number;
  hdr10?: number;
  atmos_dtsx?: number;
  internal?: number;
  nuked?: number;
  language?: number;
  rejected_language?: number;
  rejected_term?: number;
  trash_filter?: number;
  trash_tier_bonus?: number;
  total: number;
  rejected: boolean;
  rejected_reason?: string | null;
};

export type StructuralResult = {
  checked: boolean;
  matched: boolean;
  reject_reason?: string | null;
  parsed_season?: number | null;
  parsed_episode?: number | null;
  parsed_year?: number | null;
  parsed_title?: string | null;
  want_season?: number | null;
  want_episode?: number | null;
  want_year?: number | null;
};

export type MatchExplanation = {
  matched: { slot: string; value: string; alias?: string; weak?: boolean }[];
  contradictions: { slot: string; value: string }[];
  unexplained: string[];
};

export type DecisionLogRelease = {
  id: string;
  provider_call_id: string | null;
  release_title: string;
  release_guid: string | null;
  release_size: number | null;
  protocol: string | null;
  indexer_name: string | null;
  match_score: number | null;
  matched_pattern: string | null;
  match_mode: string | null;
  quality_score: number | null;
  quality_score_breakdown: QualityScoreBreakdown | null;
  total_score: number | null;
  parsed_quality: Record<string, unknown> | null;
  structural_result: StructuralResult | null;
  match_explanation: MatchExplanation | null;
  decision: string;
  decision_reason: string | null;
  episode_key: string | null;
  is_best: boolean;
  release_data: unknown | null;
};

export type DecisionLogSessionDetail = DecisionLogSession & {
  provider_calls: DecisionLogProviderCall[];
  releases: DecisionLogRelease[];
};

export type DecisionLogSettings = {
  search_logging_enabled: boolean;
  feed_logging_enabled: boolean;
  debrid_logging_enabled: boolean;
};

export type AliasScope = 'tv_show' | 'title' | 'sport_want' | 'sport_league' | 'sport_location';

export type LearnedAlias = {
  id: string;
  scope: AliasScope;
  scope_key: string;
  scope_label: string;
  alias: string;
  created_at: string;
};

export type ReviewCandidate = {
  release_key: string;
  title: string;
  size: number | null;
  published_at: string | null;
  indexer_source_id: string | null;
  indexer: string;
  protocol: string;
  download_url: string | null;
  magnet_url: string | null;
  info_hash: string | null;
  similarity: number;
  explanation: MatchExplanation | null;
  suggested_alias: string | null;
};

export type ReviewGrab = {
  watch_match_id: string;
  release_key: string;
  title: string;
  match_score: number;
  matched_pattern: string | null;
  explanation: MatchExplanation | null;
  unexplained: string[];
  runner_up: { watch_pattern_id: string; want: string; match_score: number } | null;
  suggested_alias: string | null;
};

export type ReviewItem = {
  id: string;
  kind: 'missed' | 'doubtful';
  watch_pattern_id: string;
  source_system: string;
  source_id: string;
  created_at: string;
  detail: {
    want: string;
    media_kind: string;
    due_at: string | null;
    candidates: ReviewCandidate[];
    grab: ReviewGrab | null;
  };
  scopes: { scope: AliasScope; label: string }[];
};
