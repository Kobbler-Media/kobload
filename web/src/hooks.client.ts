import type { HandleClientError } from '@sveltejs/kit';

export const handleError: HandleClientError = ({ error, message }) => {
  console.error('[SvelteKit unhandled error]', message, error);
};

if (typeof window !== 'undefined') {
  window.addEventListener('error', (event) => {
    console.error('[Uncaught error]', event.message, event.error);
  });

  window.addEventListener('unhandledrejection', (event) => {
    console.error('[Unhandled promise rejection]', event.reason);
  });
}
