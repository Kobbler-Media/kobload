/**
 * App-wide toast notification store.
 * Import `showToast` from any component to display a brief message.
 */

let message = $state('');
let timer: ReturnType<typeof setTimeout> | null = null;

export function showToast(msg: string, durationMs = 3000) {
  if (timer) clearTimeout(timer);
  message = msg;
  timer = setTimeout(() => {
    message = '';
    timer = null;
  }, durationMs);
}

export function getToastMessage(): string {
  return message;
}
