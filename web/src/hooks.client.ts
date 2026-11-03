import type { HandleClientError } from '@sveltejs/kit/hooks';

export const handleError: HandleClientError = ({ kind, error }) => {
  console.error(`[SvelteKit ${kind} error]`, error);
};

if (typeof window !== 'undefined') {
  window.addEventListener('error', (event) => {
    console.error('[Uncaught error]', event.message, event.error);
  });

  window.addEventListener('unhandledrejection', (event) => {
    console.error('[Unhandled promise rejection]', event.reason);
  });
}
