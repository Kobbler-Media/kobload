<script lang="ts">
  import { untrack } from 'svelte';
  import { Modal, SearchSelect, type SearchSelectItem } from '@koblibs/ui';
  import { updateWatchPattern } from '#lib/api/client.js';
  import { discardGuard } from '#lib/discardGuard.js';
  import type { WatchPattern } from '#lib/types.js';
  import { getPatternTitle } from './grouping';

  let {
    pattern,
    onClose,
    onSaved
  }: {
    pattern: WatchPattern;
    onClose: () => void;
    onSaved: () => void;
  } = $props();

  let editForm = $state(
    untrack(() => ({
      patterns: pattern.patterns.join('\n'),
      match_mode: pattern.match_mode,
      min_score: pattern.min_score ?? 0.75,
      enabled: pattern.enabled
    }))
  );
  let saving = $state(false);
  let error = $state('');
  const canClose = discardGuard(editForm);
  const cancel = () => {
    if (canClose()) onClose();
  };

  const formId = 'watch-pattern-edit-form';

  const matchModeItems: SearchSelectItem[] = [
    { value: 'fuzzy', label: 'Fuzzy' },
    { value: 'exact', label: 'Exact' },
    { value: 'contains', label: 'Contains' }
  ];

  const save = async () => {
    saving = true;
    error = '';
    try {
      const patternsList = editForm.patterns
        .split('\n')
        .map((p) => p.trim())
        .filter((p) => p.length > 0);
      await updateWatchPattern(pattern.id, {
        patterns: patternsList,
        match_mode: editForm.match_mode,
        min_score: editForm.min_score,
        enabled: editForm.enabled
      });
      onSaved();
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to update pattern';
    } finally {
      saving = false;
    }
  };
</script>

<Modal title="Edit Pattern" sheet {onClose} {canClose}>
  <p class="muted">{getPatternTitle(pattern)}</p>

  {#if error}
    <div class="error">{error}</div>
  {/if}

  <form
    id={formId}
    onsubmit={(e) => {
      e.preventDefault();
      save();
    }}
  >
    <label>
      Patterns (one per line)
      <textarea bind:value={editForm.patterns} rows="5"></textarea>
    </label>

    <div class="form-row">
      <label>
        Match Mode
        <SearchSelect
          id="match-mode"
          items={matchModeItems}
          value={editForm.match_mode}
          onchange={(v) => {
            editForm.match_mode = v;
          }}
          placeholder="Select..."
        />
      </label>
      <label>
        Min Score
        <input type="number" bind:value={editForm.min_score} min="0" max="1" step="0.05" />
      </label>
    </div>

    <label class="checkbox">
      <input type="checkbox" bind:checked={editForm.enabled} />
      Enabled
    </label>
  </form>

  {#snippet footer()}
    <button class="btn compact alt" type="button" onclick={cancel} disabled={saving}>Cancel</button>
    <button class="btn compact" type="submit" form={formId} disabled={saving}>Save</button>
  {/snippet}
</Modal>

<style>
  .muted {
    color: var(--muted);
    font-size: 14px;
    margin: 0;
  }

  .error {
    padding: 12px;
    border-radius: 12px;
    margin-bottom: 16px;
    font-size: 13px;
    background: rgba(220, 53, 69, 0.15);
    border: 1px solid rgba(220, 53, 69, 0.3);
    color: #dc3545;
  }

  form {
    margin-top: 16px;
  }

  label {
    display: flex;
    flex-direction: column;
    gap: 6px;
    font-size: 12px;
    color: var(--muted);
    margin-bottom: 12px;
  }

  input,
  textarea {
    padding: 10px 14px;
    border-radius: 10px;
    border: 1px solid var(--ring);
    background: var(--bg);
    color: var(--ink);
    font-size: 13px;
  }

  textarea {
    font-family: monospace;
    resize: vertical;
  }

  .form-row {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 12px;
  }

  .checkbox {
    flex-direction: row;
    align-items: center;
    gap: 8px;
  }
</style>
