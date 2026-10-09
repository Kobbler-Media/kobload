import { clearAuth, getAuthToken } from '$lib/stores/auth.svelte';

const apiBase = import.meta.env.VITE_API_BASE_URL || '';

export class ApiError extends Error {
  constructor(
    public status: number,
    message: string
  ) {
    super(message);
    this.name = 'ApiError';
  }
}

type FetchOptions = Parameters<typeof fetch>[1];

async function request<T>(path: string, options: FetchOptions = {}): Promise<T> {
  const token = getAuthToken();
  const response = await fetch(`${apiBase}${path}`, {
    ...options,
    headers: {
      'Content-Type': 'application/json',
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
      ...options.headers
    }
  });

  if (!response.ok) {
    if (response.status === 401) {
      clearAuth();
    }
    let message = `Request failed: ${response.status}`;
    try {
      const error = await response.json();
      if (typeof error?.message === 'string' && error.message.trim()) {
        message = error.message.trim();
      }
    } catch {
      try {
        const text = await response.text();
        if (text.trim()) {
          message = text.trim();
        } else if (response.statusText) {
          message = response.statusText;
        }
      } catch {
        if (response.statusText) {
          message = response.statusText;
        }
      }
    }
    throw new ApiError(response.status, message);
  }

  if (response.status === 204) {
    return undefined as T;
  }

  return response.json();
}

export type DownloadFileInfo = {
  relative_path: string;
  size: number;
  extension: string;
};

export type DownloadItem = {
  id: string;
  name: string;
  protocol: string;
  status: string;
  progress: number | null;
  last_poll_at: string | null;
  last_poll_error: string | null;
  created_at: string;
  updated_at: string;
  // Extended fields
  download_client_id: string | null;
  external_id: string | null;
  file_path: string | null;
  error: string | null;
  callback_sent_at: string | null;
  import_confirmed_at: string | null;
  files_json: DownloadFileInfo[] | null;
  release_payload: Record<string, unknown> | null;
  /** Client-provided metadata blob (echoed back in callbacks) */
  metadata: Record<string, unknown>;
  // Callback retry tracking
  callback_attempts: number;
  callback_last_attempt_at: string | null;
  callback_error: string | null;
  /** Recoverable error scanning files (e.g., network share not mounted) */
  files_scan_error: string | null;
  /** Raw data from download client for debugging */
  client_data: Record<string, unknown> | null;
  // Joined fields
  download_client_name: string | null;
  api_key_name: string | null;
  watch_pattern_source_system: string | null;
  watch_pattern_source_id: string | null;
};

export type DownloadEvent = {
  id: string;
  event_type: string;
  message: string | null;
  details: Record<string, unknown> | null;
  created_at: string;
};

export type DownloadClientConfig = {
  id: string;
  name: string;
  client_type: string;
  protocol: string;
  base_url: string;
  api_key: string | null;
  username: string | null;
  password: string | null;
  category: string | null;
  download_dir: string | null;
  is_default: boolean;
  delete_after_import: boolean;
  seed_ratio: number | null;
  seed_time_minutes: number | null;
  enabled: boolean;
  created_at: string;
  updated_at: string;
};

export type MediaType = {
  id: string;
  key: string;
  name: string;
  protocol_priority: string[];
  protocol_delay_minutes: number | null;
  created_at: string;
  updated_at: string;
};

export type ConfigResponse = {
  download_clients: DownloadClientConfig[];
};

export type UserSummary = {
  id: string;
  email: string;
  created_at: string;
};

export async function login(email: string, password: string): Promise<{ token: string }> {
  return request('/api/v1/admin/login', {
    method: 'POST',
    body: JSON.stringify({ email, password })
  });
}

export async function listUsers(): Promise<UserSummary[]> {
  return request('/api/v1/admin/users');
}

export async function createUser(payload: {
  email: string;
  password: string;
}): Promise<{ user_id: string }> {
  return request('/api/v1/admin/users', {
    method: 'POST',
    body: JSON.stringify(payload)
  });
}

export async function updateUser(
  userId: string,
  payload: {
    email?: string;
    password?: string;
  }
): Promise<{ user_id: string; email: string }> {
  return request(`/api/v1/admin/users/${userId}`, {
    method: 'PATCH',
    body: JSON.stringify(payload)
  });
}

export type PaginatedDownloads = { items: DownloadItem[]; next_cursor: string | null };

export async function listDownloads(params?: {
  status?: string;
  search?: string;
  cursor?: string;
  limit?: number;
}): Promise<PaginatedDownloads> {
  const searchParams = new URLSearchParams();
  if (params?.status) searchParams.set('status', params.status);
  if (params?.search) searchParams.set('search', params.search);
  if (params?.cursor) searchParams.set('cursor', params.cursor);
  if (params?.limit) searchParams.set('limit', String(params.limit));
  const qs = searchParams.toString();
  return request(`/api/v1/downloads${qs ? `?${qs}` : ''}`);
}

export async function createDownload(payload: {
  source: string;
  protocol: string;
  name: string;
  download_url: string;
  download_client_id?: string | null;
  callback_url: string;
  auth_token?: string | null;
  metadata?: Record<string, unknown>;
  release?: Record<string, unknown> | null;
  media_kind?: string;
}): Promise<{ id: string; status: string }> {
  return request('/api/v1/downloads', {
    method: 'POST',
    body: JSON.stringify({
      ...payload,
      metadata: payload.metadata ?? {},
      release: payload.release ?? null
    })
  });
}

export async function getDownloadEvents(id: string): Promise<DownloadEvent[]> {
  return request(`/api/v1/downloads/${id}/events`);
}

export async function deleteDownload(id: string, cancel: boolean = false): Promise<void> {
  await request(`/api/v1/downloads/${id}?cancel=${cancel}`, {
    method: 'DELETE'
  });
}

export async function clearDownloads(cancelProtocols?: string[]): Promise<{ deleted: number }> {
  const params = cancelProtocols?.length ? `?cancel_protocols=${cancelProtocols.join(',')}` : '';
  return request(`/api/v1/downloads${params}`, {
    method: 'DELETE'
  });
}

export type ReingestResponse = {
  success: boolean;
  message: string;
};

export async function reingestDownload(
  id: string,
  options?: { callback_url?: string; auth_token?: string }
): Promise<ReingestResponse> {
  return request(`/api/v1/downloads/${id}/reingest`, {
    method: 'POST',
    body: JSON.stringify(options ?? {})
  });
}

export type RescanResponse = {
  success: boolean;
  message: string;
  files_count: number;
};

export async function rescanDownload(id: string): Promise<RescanResponse> {
  return request(`/api/v1/downloads/${id}/rescan`, {
    method: 'POST'
  });
}

export async function getConfig(): Promise<ConfigResponse> {
  return request('/api/v1/admin/config');
}

export async function listDownloadClients(): Promise<DownloadClientConfig[]> {
  return request('/api/v1/admin/download-clients');
}

export async function createDownloadClient(payload: {
  name: string;
  client_type: string;
  protocol: string;
  base_url: string;
  api_key?: string | null;
  username?: string | null;
  password?: string | null;
  category?: string | null;
  download_dir?: string | null;
  is_default?: boolean;
  delete_after_import?: boolean;
  seed_ratio?: number | null;
  seed_time_minutes?: number | null;
  enabled?: boolean;
}): Promise<void> {
  await request('/api/v1/admin/download-clients', {
    method: 'POST',
    body: JSON.stringify(payload)
  });
}

export async function updateDownloadClient(
  clientId: string,
  payload: {
    name?: string;
    base_url?: string;
    api_key?: string | null;
    username?: string | null;
    password?: string | null;
    category?: string | null;
    download_dir?: string | null;
    is_default?: boolean;
    delete_after_import?: boolean;
    seed_ratio?: number | null;
    seed_time_minutes?: number | null;
    enabled?: boolean;
  }
): Promise<void> {
  await request(`/api/v1/admin/download-clients/${clientId}`, {
    method: 'PATCH',
    body: JSON.stringify(payload)
  });
}

export async function deleteDownloadClient(clientId: string): Promise<void> {
  await request(`/api/v1/admin/download-clients/${clientId}`, {
    method: 'DELETE'
  });
}

export async function listMediaTypes(): Promise<MediaType[]> {
  return request('/api/v1/media-types');
}

export async function updateMediaType(
  id: string,
  data: { name?: string; protocol_priority?: string[]; protocol_delay_minutes?: number | null }
): Promise<void> {
  return request(`/api/v1/media-types/${id}`, {
    method: 'PATCH',
    body: JSON.stringify(data)
  });
}

// Handler Type Defaults (category mappings per handler + media type)
export type HandlerTypeDefault = {
  id: string;
  handler_type: string;
  media_type_id: string;
  category_ids: string[];
};

export async function listHandlerTypeDefaults(mediaTypeId: string): Promise<HandlerTypeDefault[]> {
  return request(`/api/v1/media-types/${mediaTypeId}/defaults`);
}

export async function upsertHandlerTypeDefault(
  mediaTypeId: string,
  data: { handler_type: string; category_ids: string[] }
): Promise<void> {
  return request(`/api/v1/media-types/${mediaTypeId}/defaults`, {
    method: 'POST',
    body: JSON.stringify(data)
  });
}

// Protocol Defaults
export type ProtocolDefaults = {
  protocol_priority: string[];
  protocol_delay_minutes: number | null;
};

export async function getProtocolDefaults(): Promise<ProtocolDefaults> {
  return request('/api/v1/admin/protocol-defaults');
}

export async function setProtocolDefaults(data: ProtocolDefaults): Promise<void> {
  return request('/api/v1/admin/protocol-defaults', {
    method: 'PUT',
    body: JSON.stringify(data)
  });
}

// API Keys
export type ApiKeySummary = {
  id: string;
  name: string;
  key: string | null;
  last_used_at: string | null;
  created_at: string;
  revoked: boolean;
};

export async function listApiKeys(): Promise<ApiKeySummary[]> {
  return request('/api/v1/admin/api-keys');
}

export async function createApiKey(name: string): Promise<{ id: string; key: string }> {
  return request('/api/v1/admin/api-keys', {
    method: 'POST',
    body: JSON.stringify({ name })
  });
}

export async function revokeApiKey(id: string): Promise<void> {
  await request(`/api/v1/admin/api-keys/${id}`, {
    method: 'DELETE'
  });
}

// Connection testing
export type TestConnectionResult = {
  success: boolean;
  message: string;
};

export async function testProwlarrConnection(config: {
  base_url: string;
  api_key: string;
}): Promise<TestConnectionResult> {
  return request('/api/v1/admin/test-prowlarr', {
    method: 'POST',
    body: JSON.stringify(config)
  });
}

export async function testDownloadClientConnection(
  clientId: string,
  config?: {
    client_type?: string;
    base_url?: string;
    api_key?: string;
    username?: string;
    password?: string;
  }
): Promise<TestConnectionResult> {
  return request(`/api/v1/admin/download-clients/${clientId}/test`, {
    method: 'POST',
    body: JSON.stringify(config ?? {})
  });
}

// Test download client config without saving (for new/unsaved clients)
// Accepts client_type plus any fields defined for that type
export async function testDownloadClientConfig(
  config: { client_type: string } & Record<string, unknown>
): Promise<TestConnectionResult> {
  return request('/api/v1/admin/download-clients/test', {
    method: 'POST',
    body: JSON.stringify(config)
  });
}

// Download client types
export type DownloadClientField = {
  name: string;
  label: string;
  field_type: 'text' | 'password' | 'number' | 'checkbox';
  placeholder?: string;
  required?: boolean;
};

export type DownloadClientTypeInfo = {
  id: string;
  name: string;
  protocol: string;
  fields: DownloadClientField[];
};

export async function listDownloadClientTypes(): Promise<DownloadClientTypeInfo[]> {
  return request('/api/v1/admin/download-client-types');
}

// Dev Snapshot
export type DevSnapshotStatus = {
  exists: boolean;
  size: number | null;
  created_at: string | null;
  dev_mode: boolean;
  koblem_enabled: boolean;
};

export type DevSnapshotSaveResult = {
  success: boolean;
  message: string;
  table_count: number;
};

export async function getDevSnapshotStatus(): Promise<DevSnapshotStatus> {
  return request('/api/v1/admin/dev/snapshot');
}

export async function saveDevSnapshot(): Promise<DevSnapshotSaveResult> {
  return request('/api/v1/admin/dev/snapshot/save', {
    method: 'POST'
  });
}

export async function deleteDevSnapshot(): Promise<void> {
  await request('/api/v1/admin/dev/snapshot', {
    method: 'DELETE'
  });
}

// Watch Patterns
import type {
  WatchPattern,
  WatchDirectory,
  WatchDirectoryWithStats,
  WatchDirectoryFile,
  DirectoryEntry,
  IndexerSource,
  IndexerCategoryMapping,
  IndexerRssCacheInfo,
  WatchMatch,
  WatchPatternDetail,
  SearchAndDownloadResponse,
  QualityProfile,
  MediaWant,
  PaginatedResponse
} from '$lib/types';

export async function listWatchPatterns(params?: {
  source_system?: string;
  enabled?: boolean;
  media_type_id?: string;
  cursor?: string;
  limit?: number;
  search?: string;
}): Promise<PaginatedResponse<WatchPattern>> {
  const searchParams = new URLSearchParams();
  if (params?.source_system) searchParams.set('source_system', params.source_system);
  if (params?.enabled !== undefined) searchParams.set('enabled', String(params.enabled));
  if (params?.media_type_id) searchParams.set('media_type_id', params.media_type_id);
  if (params?.cursor) searchParams.set('cursor', params.cursor);
  if (params?.limit) searchParams.set('limit', String(params.limit));
  if (params?.search) searchParams.set('search', params.search);
  const query = searchParams.toString();
  return request(`/api/v1/watch${query ? `?${query}` : ''}`);
}

export async function updateWatchPattern(
  id: string,
  data: {
    patterns?: string[];
    match_mode?: string;
    min_score?: number;
    enabled?: boolean;
  }
): Promise<WatchPattern> {
  return request(`/api/v1/watch/${id}`, {
    method: 'PUT',
    body: JSON.stringify(data)
  });
}

export async function patchWatchPattern(
  id: string,
  data: { enabled?: boolean }
): Promise<WatchPattern> {
  return request(`/api/v1/watch/${id}`, {
    method: 'PATCH',
    body: JSON.stringify(data)
  });
}

export async function deleteWatchPattern(sourceSystem: string, sourceId: string): Promise<void> {
  await request(`/api/v1/watch/${sourceSystem}/${sourceId}`, {
    method: 'DELETE'
  });
}

export async function getPatternDetail(
  sourceSystem: string,
  sourceId: string
): Promise<WatchPatternDetail> {
  return request(`/api/v1/watch/${sourceSystem}/${sourceId}/detail`);
}

export async function searchPatternNow(
  sourceSystem: string,
  sourceId: string
): Promise<{ found: boolean }> {
  return request(`/api/v1/watch/${sourceSystem}/${sourceId}/search-now`, { method: 'POST' });
}

export async function grabHeldMatch(
  sourceSystem: string,
  sourceId: string
): Promise<{ download_id: string }> {
  return request(`/api/v1/watch/${sourceSystem}/${sourceId}/grab-held`, { method: 'POST' });
}

// Indexer Sources
export async function listIndexerSources(): Promise<IndexerSource[]> {
  return request('/api/v1/indexer-sources');
}

export type IndexerHandlerInfo = {
  id: string;
  name: string;
  config_schema: {
    fields?: {
      name: string;
      label: string;
      type: string;
      required?: boolean;
      placeholder?: string;
      default?: unknown;
      options?: Record<string, string>;
      help?: string;
      help_url?: string;
    }[];
  };
};

export async function listIndexerHandlers(): Promise<IndexerHandlerInfo[]> {
  return request('/api/v1/admin/indexer-handlers');
}

export async function testIndexerConnection(
  handlerType: string,
  handlerConfig: Record<string, unknown>
): Promise<{ success: boolean; message: string }> {
  return request('/api/v1/admin/test-indexer', {
    method: 'POST',
    body: JSON.stringify({ handler_type: handlerType, handler_config: handlerConfig })
  });
}

export async function createIndexerSource(data: {
  name: string;
  handler_type: string;
  handler_config: Record<string, unknown>;
  category_ids?: number[] | null;
  media_type_ids?: string[] | null;
  enabled?: boolean;
  priority?: number;
  seed_ratio?: number | null;
  seed_time_minutes?: number | null;
  search_mode?: string;
  debrid_mode?: string;
  auto_throttle?: boolean;
  api_limit_daily?: number | null;
}): Promise<IndexerSource> {
  return request('/api/v1/indexer-sources', {
    method: 'POST',
    body: JSON.stringify(data)
  });
}

export async function updateIndexerSource(
  id: string,
  data: {
    name: string;
    handler_type: string;
    handler_config: Record<string, unknown>;
    category_ids?: number[] | null;
    media_type_ids?: string[] | null;
    enabled?: boolean;
    priority?: number;
    seed_ratio?: number | null;
    seed_time_minutes?: number | null;
    search_mode?: string;
    debrid_mode?: string;
    auto_throttle?: boolean;
    api_limit_daily?: number | null;
  }
): Promise<IndexerSource> {
  return request(`/api/v1/indexer-sources/${id}`, {
    method: 'PUT',
    body: JSON.stringify(data)
  });
}

export async function deleteIndexerSource(id: string): Promise<void> {
  await request(`/api/v1/indexer-sources/${id}`, {
    method: 'DELETE'
  });
}

export async function triggerIndexerCheck(id: string): Promise<{ status: string }> {
  return request(`/api/v1/indexer-sources/${id}/check`, {
    method: 'POST'
  });
}

export async function resetIndexerHealth(id: string): Promise<void> {
  return request(`/api/v1/indexer-sources/${id}/reset-health`, {
    method: 'POST'
  });
}

export type SyncProwlarrResponse = {
  created: number;
  updated: number;
  disabled: number;
  indexers: string[];
};

export async function syncProwlarrIndexers(): Promise<SyncProwlarrResponse> {
  return request('/api/v1/indexer-sources/sync-prowlarr', {
    method: 'POST'
  });
}

export async function listIndexerCategoryMappings(
  sourceId: string
): Promise<IndexerCategoryMapping[]> {
  return request(`/api/v1/admin/indexer-category-mappings/${sourceId}`);
}

export async function createIndexerCategoryMapping(data: {
  indexer_source_id: string;
  category_key: string;
  indexer_category_id: number;
}): Promise<{ id: string }> {
  return request('/api/v1/admin/indexer-category-mappings', {
    method: 'POST',
    body: JSON.stringify(data)
  });
}

export async function deleteIndexerCategoryMappings(sourceId: string): Promise<void> {
  await request(`/api/v1/admin/indexer-category-mappings/${sourceId}`, {
    method: 'DELETE'
  });
}

export async function getIndexerRssCacheInfo(sourceId: string): Promise<IndexerRssCacheInfo> {
  return request(`/api/v1/admin/indexer-rss-cache/${sourceId}`);
}

// Cardigann Definitions
export type DefinitionSummary = {
  name: string;
  description: string;
  site_type: string | null;
  language: string | null;
  links: string[];
  settings: {
    name: string;
    setting_type: string;
    label: string | null;
    default: string | null;
    options: Record<string, string> | null;
    info_description: string | null;
  }[];
};

export async function listDefinitions(): Promise<DefinitionSummary[]> {
  return request('/api/v1/admin/definitions');
}

export async function testDefinition(
  name: string,
  config: Record<string, string>
): Promise<{ status?: string; error?: string }> {
  return request(`/api/v1/admin/definitions/${name}/test`, {
    method: 'POST',
    body: JSON.stringify({ config })
  });
}

export async function updateDefinitions(): Promise<{ downloaded: number; total: number }> {
  return request('/api/v1/admin/definitions/update', { method: 'POST' });
}

// Debrid Providers

export interface DebridProvider {
  id: string;
  name: string;
  provider_type: string;
  api_key: string;
  enabled: boolean;
  priority: number;
  created_at: string;
  updated_at: string;
}

export interface DebridTestResult {
  success: boolean;
  message: string;
  account?: {
    username?: string;
    expiration?: string;
    premium?: boolean;
    remaining_traffic?: string;
  };
}

export async function getDebridProviders(): Promise<DebridProvider[]> {
  return request('/api/v1/admin/debrid/providers');
}

export async function createDebridProvider(provider: {
  name: string;
  provider_type: string;
  api_key: string;
  enabled?: boolean;
  priority?: number;
}): Promise<{ id: string }> {
  return request('/api/v1/admin/debrid/providers', {
    method: 'POST',
    body: JSON.stringify(provider)
  });
}

export async function updateDebridProvider(
  id: string,
  updates: Partial<{
    name: string;
    provider_type: string;
    api_key: string;
    enabled: boolean;
    priority: number;
  }>
): Promise<void> {
  return request(`/api/v1/admin/debrid/providers/${id}`, {
    method: 'PUT',
    body: JSON.stringify(updates)
  });
}

export async function deleteDebridProvider(id: string): Promise<void> {
  return request(`/api/v1/admin/debrid/providers/${id}`, { method: 'DELETE' });
}

export async function testDebridProvider(config: {
  provider_type: string;
  api_key: string;
}): Promise<DebridTestResult> {
  return request('/api/v1/admin/debrid/providers/test', {
    method: 'POST',
    body: JSON.stringify(config)
  });
}

export async function testDebridProviderById(id: string): Promise<DebridTestResult> {
  return request(`/api/v1/admin/debrid/providers/${id}/test`, { method: 'POST' });
}

// Debrid - Public Tracker Setup
export async function setupPublicTrackers(): Promise<{
  created: number;
  skipped: number;
  details: string[];
}> {
  return request('/api/v1/admin/debrid/setup-public-trackers', { method: 'POST' });
}

// Hash Index
export type HashIndexStats = {
  total: number;
  by_source: Record<string, number>;
};

export async function getHashIndexStats(): Promise<HashIndexStats> {
  return request('/api/v1/admin/debrid/hash-index/stats');
}

export async function triggerHashIndexImport(source: string): Promise<{ job_id: string }> {
  return request('/api/v1/admin/debrid/hash-index/import', {
    method: 'POST',
    body: JSON.stringify({ source })
  });
}

export async function bootstrapHashIndex(
  file: File
): Promise<{ job_id: string; file_size_mb: string }> {
  const token = getAuthToken();
  const formData = new FormData();
  formData.append('file', file);

  const response = await fetch(`${apiBase}/api/v1/admin/debrid/hash-index/bootstrap`, {
    method: 'POST',
    headers: {
      ...(token ? { Authorization: `Bearer ${token}` } : {})
      // Don't set Content-Type — browser sets it with boundary for multipart
    },
    body: formData
  });

  if (!response.ok) {
    if (response.status === 401) {
      clearAuth();
    }
    const text = await response.text();
    throw new ApiError(response.status, text);
  }

  return response.json();
}

// Watch Matches
export async function listWatchMatches(params?: {
  watch_pattern_id?: string;
  action?: string;
  rejected?: boolean;
  cursor?: string;
  limit?: number;
  search?: string;
}): Promise<PaginatedResponse<WatchMatch>> {
  const searchParams = new URLSearchParams();
  if (params?.watch_pattern_id) searchParams.set('watch_pattern_id', params.watch_pattern_id);
  if (params?.action) searchParams.set('action', params.action);
  if (params?.rejected !== undefined) searchParams.set('rejected', String(params.rejected));
  if (params?.cursor) searchParams.set('cursor', params.cursor);
  if (params?.limit) searchParams.set('limit', String(params.limit));
  if (params?.search) searchParams.set('search', params.search);
  const query = searchParams.toString();
  return request(`/api/v1/watch-matches${query ? `?${query}` : ''}`);
}

// Search and Download (for manual search from watch UI)
export async function searchAndDownload(data: {
  patterns: string[];
  media_want?: MediaWant | null;
  media_type?: string;
  auto_download?: boolean;
  min_score?: number;
  metadata?: Record<string, unknown>;
}): Promise<SearchAndDownloadResponse> {
  return request('/api/v1/search-and-download', {
    method: 'POST',
    body: JSON.stringify(data)
  });
}

export async function listQualityProfiles(): Promise<QualityProfile[]> {
  return request('/api/v1/admin/quality-profiles');
}

export async function createQualityProfile(
  payload: Partial<QualityProfile> & { name: string; media_kind: string }
): Promise<{ id: string }> {
  return request('/api/v1/admin/quality-profiles', {
    method: 'POST',
    body: JSON.stringify(payload)
  });
}

export async function updateQualityProfile(
  id: string,
  payload: Partial<QualityProfile>
): Promise<void> {
  await request(`/api/v1/admin/quality-profiles/${id}`, {
    method: 'PATCH',
    body: JSON.stringify(payload)
  });
}

export async function deleteQualityProfile(id: string): Promise<void> {
  await request(`/api/v1/admin/quality-profiles/${id}`, {
    method: 'DELETE'
  });
}

export async function resetQualityProfiles(): Promise<QualityProfile[]> {
  return request('/api/v1/admin/quality-profiles/reset', {
    method: 'POST'
  });
}

// Dashboard
export type DashboardSummary = {
  recent_downloads: DownloadItem[];
  recent_matches: WatchMatch[];
  recent_patterns: WatchPattern[];
};

export async function getDashboardSummary(): Promise<DashboardSummary> {
  return request('/api/v1/admin/dashboard/summary');
}

// System Health
export type IndexerHealth = {
  id: string;
  name: string;
  enabled: boolean;
  consecutive_failures: number;
  health_status: string;
  last_success_at: string | null;
  last_failure_at: string | null;
  last_error: string | null;
};

export type SystemHealthResponse = {
  indexers: IndexerHealth[];
  active_patterns: number;
  total_indexers: number;
  healthy_indexers: number;
  degraded_indexers: number;
  unhealthy_indexers: number;
};

export async function getSystemHealth(): Promise<SystemHealthResponse> {
  return request('/api/v1/admin/system/health');
}

// Watch pattern test (dry-run) — returns session_id for decision log detail page
export async function testWatchPattern(
  id: string,
  releaseString?: string
): Promise<{ session_id: string }> {
  return request(`/api/v1/admin/watch/${id}/test`, {
    method: 'POST',
    body: JSON.stringify(releaseString ? { release_string: releaseString } : {})
  });
}

// Decision Logs
export async function getDecisionLogSessions(params?: {
  trigger_type?: string;
  is_test?: boolean;
  watch_pattern_id?: string;
  limit?: number;
  offset?: number;
}): Promise<import('$lib/types').DecisionLogSession[]> {
  const searchParams = new URLSearchParams();
  if (params?.trigger_type) searchParams.set('trigger_type', params.trigger_type);
  if (params?.is_test !== undefined) searchParams.set('is_test', String(params.is_test));
  if (params?.watch_pattern_id) searchParams.set('watch_pattern_id', params.watch_pattern_id);
  if (params?.limit) searchParams.set('limit', String(params.limit));
  if (params?.offset) searchParams.set('offset', String(params.offset));
  const qs = searchParams.toString();
  return request(`/api/v1/decision-logs/sessions${qs ? `?${qs}` : ''}`);
}

export async function getDecisionLogSession(
  id: string
): Promise<import('$lib/types').DecisionLogSessionDetail> {
  return request(`/api/v1/decision-logs/sessions/${id}`);
}

export async function exportDecisionLogSession(id: string): Promise<unknown> {
  return request(`/api/v1/decision-logs/sessions/${id}/export`);
}

export async function deleteDecisionLogSession(id: string): Promise<void> {
  await request(`/api/v1/decision-logs/sessions/${id}`, { method: 'DELETE' });
}

export async function deleteDecisionLogSessions(triggerType?: string): Promise<void> {
  const searchParams = new URLSearchParams();
  if (triggerType) searchParams.set('trigger_type', triggerType);
  const qs = searchParams.toString();
  await request(`/api/v1/decision-logs/sessions${qs ? `?${qs}` : ''}`, { method: 'DELETE' });
}

export async function getDecisionLogSettings(): Promise<import('$lib/types').DecisionLogSettings> {
  return request('/api/v1/decision-logs/settings');
}

export async function updateDecisionLogSettings(
  settings: Partial<import('$lib/types').DecisionLogSettings>
): Promise<void> {
  await request('/api/v1/decision-logs/settings', {
    method: 'PUT',
    body: JSON.stringify(settings)
  });
}

// Status Webhooks
export type StatusWebhook = {
  id: string;
  url: string;
  events: string[];
  enabled: boolean;
  created_at: string;
  updated_at: string;
};

export async function listWebhooks(): Promise<StatusWebhook[]> {
  return request('/api/v1/admin/webhooks');
}

export async function createWebhook(data: {
  url: string;
  secret?: string;
  events?: string[];
}): Promise<StatusWebhook> {
  return request('/api/v1/admin/webhooks', {
    method: 'POST',
    body: JSON.stringify(data)
  });
}

export async function updateWebhook(
  id: string,
  data: { url?: string; secret?: string | null; events?: string[]; enabled?: boolean }
): Promise<void> {
  return request(`/api/v1/admin/webhooks/${id}`, {
    method: 'PUT',
    body: JSON.stringify(data)
  });
}

export async function deleteWebhook(id: string): Promise<void> {
  return request(`/api/v1/admin/webhooks/${id}`, {
    method: 'DELETE'
  });
}

// Watch Directories
export async function listWatchDirectories(): Promise<WatchDirectory[]> {
  return request('/api/v1/watch-directories');
}

export async function createWatchDirectory(data: {
  path: string;
  name: string;
  enabled?: boolean;
  recursive?: boolean;
  media_type?: string;
  callback_url: string;
  auth_token?: string | null;
}): Promise<WatchDirectory> {
  return request('/api/v1/watch-directories', {
    method: 'POST',
    body: JSON.stringify(data)
  });
}

export async function getWatchDirectory(id: string): Promise<WatchDirectoryWithStats> {
  return request(`/api/v1/watch-directories/${id}`);
}

export async function updateWatchDirectory(
  id: string,
  data: {
    name?: string;
    path?: string;
    enabled?: boolean;
    recursive?: boolean;
    media_type?: string;
    callback_url?: string;
    auth_token?: string | null;
  }
): Promise<void> {
  return request(`/api/v1/watch-directories/${id}`, {
    method: 'PATCH',
    body: JSON.stringify(data)
  });
}

export async function deleteWatchDirectory(id: string): Promise<void> {
  return request(`/api/v1/watch-directories/${id}`, {
    method: 'DELETE'
  });
}

export async function scanWatchDirectory(id: string): Promise<void> {
  return request(`/api/v1/watch-directories/${id}/scan`, {
    method: 'POST'
  });
}

export async function retryWatchDirectory(id: string): Promise<void> {
  return request(`/api/v1/watch-directories/${id}/retry`, {
    method: 'POST'
  });
}

export async function listWatchDirectoryFiles(
  dirId: string,
  params?: { limit?: number; offset?: number }
): Promise<WatchDirectoryFile[]> {
  const searchParams = new URLSearchParams();
  if (params?.limit) searchParams.set('limit', String(params.limit));
  if (params?.offset) searchParams.set('offset', String(params.offset));
  const query = searchParams.toString();
  return request(`/api/v1/watch-directories/${dirId}/files${query ? `?${query}` : ''}`);
}

// Upgrade Defaults
export type UpgradeDefaults = {
  upgrade_window_hours: number;
};

export async function getUpgradeDefaults(): Promise<UpgradeDefaults> {
  return request('/api/v1/settings/upgrade-defaults');
}

export async function setUpgradeDefaults(data: UpgradeDefaults): Promise<void> {
  return request('/api/v1/settings/upgrade-defaults', {
    method: 'PUT',
    body: JSON.stringify(data)
  });
}

// Koblem Settings
export type KoblemSettings = {
  service_url: string | null;
  enabled: boolean;
};

export async function getKoblemSettings(): Promise<KoblemSettings> {
  return request('/api/v1/settings/koblem');
}

export async function setKoblemSettings(data: KoblemSettings): Promise<void> {
  return request('/api/v1/settings/koblem', {
    method: 'PUT',
    body: JSON.stringify(data)
  });
}

export type KoblemTestResult = {
  success: boolean;
  message: string;
  llama_server?: boolean;
};

export async function testKoblemConnection(serviceUrl: string): Promise<KoblemTestResult> {
  return request('/api/v1/settings/koblem/test', {
    method: 'POST',
    body: JSON.stringify({ service_url: serviceUrl })
  });
}

// Bulk Watch Operations
export async function bulkDeleteWatchPatterns(ids: string[]): Promise<{ deleted: number }> {
  return request('/api/v1/watch/bulk-delete', {
    method: 'POST',
    body: JSON.stringify({ ids })
  });
}

export async function bulkRejectWatchMatches(
  ids: string[],
  reason?: string
): Promise<{ rejected: number }> {
  return request('/api/v1/watch-matches/bulk-reject', {
    method: 'POST',
    body: JSON.stringify({ ids, reason })
  });
}

export async function bulkDeleteWatchMatches(ids: string[]): Promise<{ deleted: number }> {
  return request('/api/v1/watch-matches/bulk-delete', {
    method: 'POST',
    body: JSON.stringify({ ids })
  });
}

// Watch Match Rejection
export async function rejectWatchMatch(id: string, reason?: string): Promise<void> {
  await request(`/api/v1/watch/matches/${id}/reject`, {
    method: 'POST',
    body: JSON.stringify({ reason: reason ?? null })
  });
}

// Filesystem Browse
export type BrowseResponse = {
  current_path: string;
  parent_path: string | null;
  directories: DirectoryEntry[];
};

export async function browseFilesystem(path?: string): Promise<BrowseResponse> {
  const searchParams = new URLSearchParams();
  if (path) searchParams.set('path', path);
  const query = searchParams.toString();
  return request(`/api/v1/filesystem/browse${query ? `?${query}` : ''}`);
}

// =============================================================================
// Prowlarr Migration
// =============================================================================

export type MigrationItemStatus =
  | { type: 'will_create' }
  | { type: 'already_exists' }
  | { type: 'unsupported'; reason: string }
  | { type: 'missing_fields'; fields: string[] };

export type IndexerMigrationItem = {
  prowlarr_id: number;
  name: string;
  handler_type: string;
  handler_config: Record<string, unknown> | null;
  category_ids: number[];
  protocol: string;
  supports_rss: boolean;
  supports_search: boolean;
  status: MigrationItemStatus;
  existing_source_id: string | null;
  existing_source_name: string | null;
};

export type DownloadClientMatch = {
  prowlarr_name: string;
  prowlarr_implementation: string;
  prowlarr_protocol: string;
  kobload_client_id: string | null;
  kobload_client_name: string | null;
  match_status: 'matched' | 'no_match';
};

export type MigrationPreview = {
  indexers: IndexerMigrationItem[];
  download_clients: DownloadClientMatch[];
};

export type MigrationConfirmItem = {
  prowlarr_id: number;
  name: string;
  handler_type: string;
  handler_config: Record<string, unknown>;
  category_ids: number[];
};

export type MigrationResult = {
  created: number;
  errors: string[];
};

export async function previewProwlarrMigration(
  prowlarrSourceId: string
): Promise<MigrationPreview> {
  return request('/api/v1/indexer-sources/migrate/preview', {
    method: 'POST',
    body: JSON.stringify({ prowlarr_source_id: prowlarrSourceId })
  });
}

export async function confirmProwlarrMigration(
  prowlarrSourceId: string,
  items: MigrationConfirmItem[]
): Promise<MigrationResult> {
  return request('/api/v1/indexer-sources/migrate/confirm', {
    method: 'POST',
    body: JSON.stringify({ prowlarr_source_id: prowlarrSourceId, items })
  });
}
