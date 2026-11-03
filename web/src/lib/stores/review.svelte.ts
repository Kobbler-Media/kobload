import { getReviewCount } from '#lib/api/client.js';

let openCount = $state(0);

export function getReviewState() {
  return {
    get openCount() {
      return openCount;
    }
  };
}

export async function refreshReviewCount(): Promise<void> {
  try {
    openCount = (await getReviewCount()).open;
  } catch {
    openCount = 0;
  }
}
