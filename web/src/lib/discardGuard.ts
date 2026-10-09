/**
 * Snapshots a form at open and returns a `canClose` guard for Modal: dismissing an
 * untouched form is silent, dismissing an edited one asks first. Pass the reactive
 * `$state` form object — the guard re-reads it on every call.
 */
export function discardGuard(form: object): () => boolean {
  const initial = JSON.stringify(form);
  return () => JSON.stringify(form) === initial || confirm('Discard unsaved changes?');
}
