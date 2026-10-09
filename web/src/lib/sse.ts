import { getAuthToken } from '$lib/stores/auth.svelte';
import {
  createSseConnection as createSseConnectionBase,
  type SseConnection,
  type SseOptions
} from '@koblibs/ui';

export type { SseConnection, SseOptions };

const apiBase = import.meta.env.VITE_API_BASE_URL || '';

/**
 * Create an SSE connection with automatic reconnection and auth.
 * Thin wrapper around @koblibs/ui SSE with kobload-specific auth config.
 */
export function createSseConnection(
  path: string,
  onEvent: (payload: Record<string, unknown>) => void,
  onError: (error: string | null) => void,
  options?: SseOptions
): SseConnection {
  return createSseConnectionBase({ getAuthToken, apiBase }, path, onEvent, onError, options);
}
