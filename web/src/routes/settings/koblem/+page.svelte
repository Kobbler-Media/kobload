<script lang="ts">
  import { onMount } from 'svelte';
  import {
    getKoblemSettings,
    setKoblemSettings,
    testKoblemConnection,
    type KoblemTestResult
  } from '$lib/api/client';

  let loading = $state(false);
  let saving = $state(false);
  let error = $state('');
  let success = $state('');
  let testing = $state(false);
  let testResult = $state<KoblemTestResult | null>(null);

  let serviceUrl = $state('');
  let enabled = $state(false);

  async function load() {
    loading = true;
    error = '';
    try {
      const settings = await getKoblemSettings();
      serviceUrl = settings.service_url ?? '';
      enabled = settings.enabled;
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to load koblem settings';
    } finally {
      loading = false;
    }
  }

  async function save() {
    saving = true;
    error = '';
    success = '';
    try {
      await setKoblemSettings({
        service_url: serviceUrl.trim() || null,
        enabled
      });
      success = 'Koblem settings saved.';
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to save koblem settings';
    } finally {
      saving = false;
    }
  }

  async function testConnection() {
    if (!serviceUrl.trim()) {
      testResult = { success: false, message: 'Enter a service URL first' };
      return;
    }
    testing = true;
    testResult = null;
    try {
      testResult = await testKoblemConnection(serviceUrl.trim());
    } catch (err) {
      testResult = {
        success: false,
        message: err instanceof Error ? err.message : 'Connection test failed'
      };
    } finally {
      testing = false;
    }
  }

  onMount(load);
</script>

<div class="page-header">
  <div>
    <h1>Koblem (LLM Review)</h1>
    <p class="muted">
      Connect to a local koblem instance for LLM-based review of search candidates. When enabled,
      koblem can veto suspicious releases that pass scoring but may be wrong (e.g. wrong show,
      mislabeled quality).
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
        Service URL
        <input type="text" bind:value={serviceUrl} placeholder="http://koblem:8090" />
        <span class="helper-text">
          URL of the koblem service. Typically runs on the same network as kobload.
        </span>
      </label>

      <div class="checkbox-row">
        <label class="checkbox">
          <input type="checkbox" bind:checked={enabled} />
          Enable LLM Review
        </label>
        <span class="helper-text">
          When enabled, search results are sent to koblem for review before download. Adds up to 5
          seconds of latency per search. Requires a running koblem instance with a loaded LLM.
        </span>
      </div>

      <div class="actions">
        <button class="btn" type="submit" disabled={saving}>
          {saving ? 'Saving...' : 'Save'}
        </button>
        <button
          class="btn alt"
          type="button"
          onclick={testConnection}
          disabled={testing || !serviceUrl.trim()}
        >
          {testing ? 'Testing...' : 'Test Connection'}
        </button>
      </div>
    </form>

    {#if testResult}
      <div class="test-result" class:success={testResult.success} class:error={!testResult.success}>
        <div class="test-message">{testResult.message}</div>
        {#if testResult.success && testResult.llama_server !== undefined}
          <div class="test-detail">
            LLM Server: {testResult.llama_server ? 'Ready' : 'Not ready'}
          </div>
        {/if}
      </div>
    {/if}
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

  input[type='text'] {
    padding: 10px 14px;
    border-radius: 10px;
    border: 1px solid var(--ring);
    background: var(--bg);
    color: var(--ink);
    font-size: 13px;
    max-width: 400px;
  }

  .helper-text {
    font-size: 11px;
    color: var(--muted);
    font-style: italic;
    max-width: 480px;
  }

  .checkbox-row {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-bottom: 16px;
  }

  .checkbox {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    font-size: 13px;
    color: var(--ink);
    flex-direction: row;
    margin-bottom: 0;
  }

  .checkbox input[type='checkbox'] {
    width: 16px;
    height: 16px;
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
    transition:
      background 0.15s,
      opacity 0.15s;
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

  .btn.alt:hover:not(:disabled) {
    background: rgba(255, 255, 255, 0.06);
  }

  :global([data-theme='light']) .btn.alt:hover:not(:disabled) {
    background: rgba(0, 0, 0, 0.06);
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

  .test-result {
    margin-top: 12px;
    padding: 10px 14px;
    border-radius: 10px;
    font-size: 13px;
    background: rgba(220, 53, 69, 0.15);
    border: 1px solid rgba(220, 53, 69, 0.3);
    color: #dc3545;
  }

  .test-result.success {
    background: rgba(40, 167, 69, 0.15);
    border: 1px solid rgba(40, 167, 69, 0.3);
    color: #28a745;
  }

  .test-message {
    font-weight: 500;
  }

  .test-detail {
    margin-top: 4px;
    font-size: 12px;
    opacity: 0.85;
  }
</style>
