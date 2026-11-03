<script lang="ts">
  import { onMount } from 'svelte';
  import {
    getProtocolDefaults,
    setProtocolDefaults,
    type ProtocolDefaults
  } from '#lib/api/client.js';
  import { MultiSearchSelect, type MultiSearchSelectItem } from '@koblibs/ui';

  let loading = $state(false);
  let error = $state('');
  let defaults = $state<ProtocolDefaults>({ protocol_priority: [], protocol_delay_minutes: null });
  let editing = $state(false);
  let editPriority = $state<string[]>([]);
  let editDelay = $state<number | null>(null);
  let saving = $state(false);

  const PROTOCOL_OPTIONS: MultiSearchSelectItem[] = [
    { value: 'usenet', label: 'Usenet', description: 'NZB downloads via SABnzbd' },
    { value: 'torrent', label: 'Torrent', description: 'Torrent downloads via Transmission' }
  ];

  const refresh = async () => {
    loading = true;
    error = '';
    try {
      defaults = await getProtocolDefaults();
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to load';
    } finally {
      loading = false;
    }
  };

  const startEdit = () => {
    editPriority = [...defaults.protocol_priority];
    editDelay = defaults.protocol_delay_minutes;
    editing = true;
  };

  const save = async () => {
    saving = true;
    error = '';
    try {
      await setProtocolDefaults({
        protocol_priority: editPriority,
        protocol_delay_minutes: editDelay
      });
      editing = false;
      await refresh();
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to save';
    } finally {
      saving = false;
    }
  };

  onMount(refresh);
</script>

<div class="page-header">
  <div>
    <h1>Protocol Defaults</h1>
    <p class="muted">
      Global protocol priority and delay. Individual media types can override these.
    </p>
  </div>
  <button class="btn alt" type="button" onclick={refresh} disabled={loading}>Refresh</button>
</div>

{#if error}
  <div class="error">{error}</div>
{/if}

<section class="card">
  {#if editing}
    <form
      onsubmit={(e) => {
        e.preventDefault();
        save();
      }}
    >
      <div class="form-group">
        <label for="def-priority">Protocol Priority</label>
        <MultiSearchSelect
          id="def-priority"
          items={PROTOCOL_OPTIONS}
          bind:values={editPriority}
          placeholder="Select protocols in order of preference..."
        />
        <span class="hint"
          >Selection order determines preference. First selected is tried first.</span
        >
      </div>
      <div class="form-group">
        <label for="def-delay">Fallback Delay (minutes)</label>
        <input id="def-delay" type="number" min="0" bind:value={editDelay} placeholder="e.g. 60" />
        <span class="hint">How long to wait for preferred protocol before accepting others.</span>
      </div>
      <div class="form-actions">
        <button class="btn" type="submit" disabled={saving}>
          {saving ? 'Saving...' : 'Save'}
        </button>
        <button class="btn alt" type="button" onclick={() => (editing = false)} disabled={saving}
          >Cancel</button
        >
      </div>
    </form>
  {:else if loading}
    <div class="loading">Loading...</div>
  {:else}
    <div class="card-header">
      <div>
        <h3>Current Defaults</h3>
        <p class="hint">
          {#if defaults.protocol_priority.length > 0}
            Prefer <strong>{defaults.protocol_priority[0]}</strong>
            {#if defaults.protocol_priority.length > 1}
              then {defaults.protocol_priority.slice(1).join(', ')}
            {/if}
            {#if defaults.protocol_delay_minutes}
              (wait {defaults.protocol_delay_minutes}m before fallback)
            {/if}
          {:else}
            Not configured — all protocols equal
          {/if}
        </p>
      </div>
      <button class="btn alt small" type="button" onclick={startEdit}>Edit</button>
    </div>
  {/if}
</section>

<style>
  .page-header {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 16px;
    margin-bottom: 20px;
  }

  .page-header h1 {
    margin: 0 0 6px;
    font-size: 24px;
    font-family: 'Palatino Linotype', 'Book Antiqua', Palatino, serif;
  }

  .muted {
    color: var(--muted);
    font-size: 14px;
    margin: 0;
  }

  .card {
    background: var(--card);
    border: 1px solid var(--ring);
    border-radius: 16px;
    padding: 18px;
  }

  .card h3 {
    margin: 0 0 4px;
    font-size: 16px;
  }

  .card-header {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    align-items: flex-start;
  }

  .form-group {
    margin-bottom: 14px;
  }

  .form-group label {
    display: block;
    font-size: 12px;
    font-weight: 500;
    margin-bottom: 4px;
  }

  .form-group input {
    width: 100%;
    padding: 7px 10px;
    background: var(--bg);
    border: 1px solid var(--ring);
    border-radius: 8px;
    color: var(--ink);
    font-size: 13px;
    box-sizing: border-box;
  }

  .hint {
    display: block;
    font-size: 11px;
    color: var(--muted);
    margin-top: 3px;
  }

  .form-actions {
    display: flex;
    gap: 8px;
    margin-top: 16px;
  }

  .btn {
    padding: 8px 14px;
    border-radius: 8px;
    border: 1px solid var(--accent);
    background: var(--accent);
    color: #1a1b14;
    font-size: 12px;
    font-weight: 500;
    cursor: pointer;
  }

  .btn:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }

  .btn.alt {
    background: transparent;
    border-color: var(--ring);
    color: var(--ink);
  }

  .btn.small {
    padding: 5px 10px;
    font-size: 11px;
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

  .loading {
    padding: 24px;
    text-align: center;
    color: var(--muted);
  }
</style>
