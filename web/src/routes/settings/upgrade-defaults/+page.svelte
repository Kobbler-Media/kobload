<script lang="ts">
  import { onMount } from 'svelte';
  import { getUpgradeDefaults, setUpgradeDefaults } from '#lib/api/client.js';

  let loading = $state(false);
  let saving = $state(false);
  let error = $state('');
  let success = $state('');
  let upgradeWindowHours = $state(720);

  async function load() {
    loading = true;
    error = '';
    try {
      const defaults = await getUpgradeDefaults();
      upgradeWindowHours = defaults.upgrade_window_hours;
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to load upgrade defaults';
    } finally {
      loading = false;
    }
  }

  async function save() {
    saving = true;
    error = '';
    success = '';
    try {
      await setUpgradeDefaults({ upgrade_window_hours: upgradeWindowHours });
      success = 'Upgrade defaults saved.';
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to save upgrade defaults';
    } finally {
      saving = false;
    }
  }

  onMount(load);
</script>

<div class="page-header">
  <div>
    <h1>Upgrade Defaults</h1>
    <p class="muted">
      Configure how long kobload watches for better quality after initial download.
    </p>
  </div>
</div>

{#if error}
  <div class="error">{error}</div>
{/if}

{#if success}
  <div class="success">{success}</div>
{/if}

{#if loading}
  <div class="muted">Loading...</div>
{:else}
  <section class="card">
    <form
      onsubmit={(e) => {
        e.preventDefault();
        save();
      }}
    >
      <label>
        Upgrade Window (hours)
        <input type="number" bind:value={upgradeWindowHours} min="0" step="1" placeholder="720" />
        <span class="helper-text">
          How long kobload watches for better quality after initial download. Default is 720 hours
          (30 days). Set to 0 to disable upgrades.
        </span>
      </label>

      <div class="actions">
        <button class="btn" type="submit" disabled={saving}>
          {saving ? 'Saving...' : 'Save'}
        </button>
      </div>
    </form>
  </section>
{/if}

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

  label {
    display: flex;
    flex-direction: column;
    gap: 6px;
    font-size: 12px;
    color: var(--muted);
    margin-bottom: 16px;
  }

  input {
    padding: 10px 14px;
    border-radius: 10px;
    border: 1px solid var(--ring);
    background: var(--bg);
    color: var(--ink);
    font-size: 13px;
    max-width: 300px;
  }

  .helper-text {
    font-size: 11px;
    color: var(--muted);
    font-style: italic;
    max-width: 480px;
  }

  .actions {
    display: flex;
    gap: 10px;
  }

  .btn {
    padding: 10px 18px;
    border-radius: 10px;
    border: 1px solid var(--accent);
    background: var(--accent);
    color: #1a1b14;
    font-size: 13px;
    font-weight: 500;
    cursor: pointer;
  }

  .btn:disabled {
    opacity: 0.6;
    cursor: not-allowed;
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

  .success {
    padding: 12px;
    border-radius: 12px;
    margin-bottom: 16px;
    font-size: 13px;
    background: rgba(34, 197, 94, 0.15);
    border: 1px solid rgba(34, 197, 94, 0.3);
    color: #22c55e;
  }
</style>
