import { getUsenetStatus } from '#lib/api/client.js';
import type { UsenetJobActivity, UsenetStatusResponse } from '#lib/types.js';

const POLL_MS = 2500;
const FAILURES_BEFORE_UNAVAILABLE = 2;

let latest = $state.raw<UsenetStatusResponse | null>(null);

export const usenetStatus = () => latest;

export const usenetJob = (externalId: string | null): UsenetJobActivity | undefined => {
  const response = latest;
  if (!externalId || response?.engine !== 'available') return undefined;
  return response.status.jobs.find((job) => job.id === externalId);
};

export function watchUsenetStatus(): () => void {
  let timer: ReturnType<typeof setTimeout> | undefined;
  let controller: AbortController | undefined;
  let failures = 0;

  const cancel = () => {
    clearTimeout(timer);
    controller?.abort();
    controller = undefined;
  };

  const poll = async () => {
    const current = new AbortController();
    controller = current;
    try {
      const response = await getUsenetStatus(current.signal);
      if (current.signal.aborted) return;
      latest = response;
      failures = 0;
    } catch {
      if (current.signal.aborted) return;
      failures += 1;
      if (failures >= FAILURES_BEFORE_UNAVAILABLE) latest = { engine: 'unavailable' };
    }
    timer = setTimeout(poll, POLL_MS);
  };

  const sync = () => {
    cancel();
    if (document.visibilityState === 'visible') void poll();
  };

  document.addEventListener('visibilitychange', sync);
  sync();

  return () => {
    document.removeEventListener('visibilitychange', sync);
    cancel();
    latest = null;
  };
}
