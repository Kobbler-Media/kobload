<script lang="ts">
  import { onMount } from 'svelte';
  import { listApiKeys, createApiKey, revokeApiKey, type ApiKeySummary } from '#lib/api/client.js';

  let loading = $state(false);
  let error = $state('');
  let apiKeys = $state<ApiKeySummary[]>([]);
  let apiKeyError = $state('');
  let apiKeySaving = $state(false);
  let newApiKeyName = $state('');
  let createdApiKey = $state<string | null>(null);

  const refresh = async () => {
    loading = true;
    error = '';
    try {
      apiKeys = await listApiKeys();
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to load API keys';
    } finally {
      loading = false;
    }
  };

  const handleCreateApiKey = async () => {
    apiKeySaving = true;
    apiKeyError = '';
    createdApiKey = null;
    if (!newApiKeyName.trim()) {
      apiKeySaving = false;
      apiKeyError = 'Name is required';
      return;
    }
    try {
      const result = await createApiKey(newApiKeyName.trim());
      createdApiKey = result.key;
      newApiKeyName = '';
      apiKeys = await listApiKeys();
    } catch (err) {
      apiKeyError = err instanceof Error ? err.message : 'Failed to create API key';
    } finally {
      apiKeySaving = false;
    }
  };

  const handleRevokeApiKey = async (id: string) => {
    apiKeySaving = true;
    apiKeyError = '';
    try {
      await revokeApiKey(id);
      apiKeys = await listApiKeys();
    } catch (err) {
      apiKeyError = err instanceof Error ? err.message : 'Failed to revoke API key';
    } finally {
      apiKeySaving = false;
    }
  };

  const copyToClipboard = async (text: string) => {
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      const textarea = document.createElement('textarea');
      textarea.value = text;
      document.body.appendChild(textarea);
      textarea.select();
      document.execCommand('copy');
      document.body.removeChild(textarea);
    }
  };

  onMount(() => {
    refresh();
  });
</script>

<div class="page-header">
  <div>
    <h1>API Keys</h1>
    <p class="muted">Manage API keys for external service authentication.</p>
  </div>
  <button class="btn alt" type="button" onclick={refresh} disabled={loading}>Refresh</button>
</div>

{#if error}
  <div class="error">{error}</div>
{/if}

<section class="card">
  <h2>Create API Key</h2>
  <p class="muted">
    Create API keys for external services like Kobbler to authenticate with Kobload.
  </p>

  {#if apiKeyError}
    <div class="error">{apiKeyError}</div>
  {/if}

  <form
    class="token-create-form"
    onsubmit={(e) => {
      e.preventDefault();
      handleCreateApiKey();
    }}
  >
    <input
      type="text"
      bind:value={newApiKeyName}
      placeholder="Key name (e.g., Kobbler)"
      disabled={apiKeySaving}
    />
    <button class="btn" type="submit" disabled={apiKeySaving}> Create Key </button>
  </form>

  {#if createdApiKey}
    <div class="token-callout">
      <div class="muted">New key created:</div>
      <code class="code">{createdApiKey}</code>
      <button class="btn alt" type="button" onclick={() => copyToClipboard(createdApiKey ?? '')}>
        Copy
      </button>
    </div>
  {/if}

  {#if apiKeys.length === 0}
    <div class="empty-state">No API keys created yet.</div>
  {:else}
    <div class="token-list">
      {#each apiKeys as key (key.id)}
        <div class="token-row" class:revoked={key.revoked}>
          <div class="token-info">
            <div class="token-name">{key.name}</div>
            {#if key.key}
              <code class="token-value">{key.key}</code>
            {:else}
              <span class="muted">Key not stored (created before update)</span>
            {/if}
            <div class="token-meta muted">
              Created {new Date(key.created_at).toLocaleString()}
              {#if key.last_used_at}
                · Last used {new Date(key.last_used_at).toLocaleString()}
              {/if}
              {key.revoked ? ' · Revoked' : ''}
            </div>
          </div>
          <div class="token-actions">
            {#if key.key}
              <button
                class="btn alt small"
                type="button"
                onclick={() => copyToClipboard(key.key ?? '')}
                disabled={apiKeySaving}
              >
                Copy
              </button>
            {/if}
            {#if !key.revoked}
              <button
                class="btn alt small"
                type="button"
                onclick={() => handleRevokeApiKey(key.id)}
                disabled={apiKeySaving}
              >
                Revoke
              </button>
            {/if}
          </div>
        </div>
      {/each}
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
    margin-bottom: 18px;
  }

  .card h2 {
    margin: 0 0 8px;
    font-size: 18px;
  }

  .card p.muted {
    margin-bottom: 16px;
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

  .token-create-form {
    display: flex;
    gap: 10px;
    margin-bottom: 16px;
  }

  @media (max-width: 768px) {
    .token-create-form {
      flex-direction: column;
    }

    .token-row {
      flex-direction: column;
    }

    .token-actions {
      align-self: flex-start;
    }
  }

  .token-create-form input {
    flex: 1;
    padding: 10px 14px;
    border-radius: 10px;
    border: 1px solid var(--ring);
    background: var(--bg);
    color: var(--ink);
    font-size: 13px;
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

  .btn.alt {
    background: transparent;
    border-color: var(--ring);
    color: var(--ink);
  }

  .btn.small {
    padding: 6px 10px;
    font-size: 11px;
  }

  .token-callout {
    background: rgba(34, 197, 94, 0.1);
    border: 1px solid rgba(34, 197, 94, 0.3);
    border-radius: 12px;
    padding: 12px;
    margin-bottom: 16px;
    display: grid;
    gap: 8px;
  }

  .token-callout .code {
    font-family: monospace;
    font-size: 13px;
    background: var(--ring);
    padding: 8px 12px;
    border-radius: 8px;
    word-break: break-all;
  }

  .token-list {
    display: grid;
    gap: 8px;
  }

  .token-row {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 12px;
    padding: 12px;
    background: var(--bg);
    border: 1px solid var(--ring);
    border-radius: 12px;
  }

  .token-row.revoked {
    opacity: 0.5;
  }

  .token-info {
    flex: 1;
    min-width: 0;
    display: grid;
    gap: 4px;
  }

  .token-name {
    font-weight: 600;
    font-size: 14px;
  }

  .token-value {
    font-family: monospace;
    font-size: 12px;
    background: var(--ring);
    padding: 4px 8px;
    border-radius: 6px;
    word-break: break-all;
  }

  .token-meta {
    font-size: 12px;
  }

  .token-actions {
    display: flex;
    gap: 8px;
    flex-shrink: 0;
  }

  .empty-state {
    padding: 24px;
    text-align: center;
    border: 1px dashed var(--ring);
    border-radius: 12px;
    color: var(--muted);
  }
</style>
