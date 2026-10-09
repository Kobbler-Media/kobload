<script lang="ts">
  import { untrack } from 'svelte';
  import { Modal, SearchSelect, type SearchSelectItem } from '@koblibs/ui';
  import {
    createDebridProvider,
    updateDebridProvider,
    testDebridProvider,
    type DebridProvider,
    type DebridTestResult
  } from '$lib/api/client';
  import { discardGuard } from '$lib/discardGuard';
  import DebridTestResultView from './DebridTestResultView.svelte';

  let {
    provider,
    providerTypes,
    providerDisplayNames,
    onClose,
    onSaved
  }: {
    provider: DebridProvider | null;
    providerTypes: SearchSelectItem[];
    providerDisplayNames: Record<string, string>;
    onClose: () => void;
    onSaved: () => void;
  } = $props();

  const formId = 'debrid-provider-edit-form';

  // A new provider is named after the picked service until the user renames it.
  // Seeded into the initial value too, so an untouched Add dialog stays pristine.
  let form = $state(
    untrack(() => ({
      name: provider?.name ?? providerDisplayNames['real_debrid'] ?? '',
      provider_type: provider?.provider_type ?? 'real_debrid',
      api_key: provider?.api_key ?? '',
      enabled: provider?.enabled ?? true,
      priority: provider?.priority ?? 0
    }))
  );

  let saving = $state(false);
  let formError = $state('');
  let testing = $state(false);
  let testResult = $state<DebridTestResult | null>(null);
  let showApiKey = $state(false);

  const canClose = discardGuard(form);
  const cancel = () => {
    if (canClose()) onClose();
  };

  $effect(() => {
    const displayName = providerDisplayNames[form.provider_type];
    if (provider || !displayName) return;
    const allDisplayNames = Object.values(providerDisplayNames);
    if (!form.name || allDisplayNames.includes(form.name)) {
      form.name = displayName;
    }
  });

  const save = async () => {
    if (!form.name.trim() || !form.api_key.trim()) {
      formError = 'Name and API key are required.';
      return;
    }
    saving = true;
    formError = '';
    try {
      const payload = {
        name: form.name.trim(),
        provider_type: form.provider_type,
        api_key: form.api_key.trim(),
        enabled: form.enabled,
        priority: form.priority
      };
      if (provider) {
        await updateDebridProvider(provider.id, payload);
      } else {
        await createDebridProvider(payload);
      }
      onSaved();
    } catch (err) {
      formError = err instanceof Error ? err.message : 'Failed to save debrid provider';
    } finally {
      saving = false;
    }
  };

  const test = async () => {
    if (!form.api_key.trim() || !form.provider_type) {
      testResult = { success: false, message: 'Provider type and API key are required' };
      return;
    }
    testing = true;
    testResult = null;
    try {
      testResult = await testDebridProvider({
        provider_type: form.provider_type,
        api_key: form.api_key.trim()
      });
    } catch (err) {
      testResult = { success: false, message: err instanceof Error ? err.message : 'Test failed' };
    } finally {
      testing = false;
    }
  };
</script>

<Modal
  title={provider ? `Edit ${provider.name}` : 'Add Debrid Provider'}
  sheet
  {onClose}
  {canClose}
>
  <form
    id={formId}
    class="modal-form"
    onsubmit={(e) => {
      e.preventDefault();
      save();
    }}
  >
    {#if formError}
      <div class="error">{formError}</div>
    {/if}

    <p class="intro">Connect a debrid service to enable cached torrent downloads.</p>

    <div class="form-grid">
      <label>
        Provider type
        <SearchSelect
          id="provider-type"
          items={providerTypes}
          value={form.provider_type}
          onchange={(v) => {
            form.provider_type = v;
            testResult = null;
          }}
          placeholder="Select provider..."
        />
      </label>
      <label>
        Name
        <input type="text" bind:value={form.name} placeholder="My Debrid Provider" />
      </label>
      <div class="field">
        <span>API Key</span>
        <div class="api-key-wrapper">
          {#if showApiKey}
            <input
              type="text"
              bind:value={form.api_key}
              placeholder="Paste your API key"
              autocomplete="one-time-code"
            />
          {:else}
            <input
              type="password"
              bind:value={form.api_key}
              placeholder="Paste your API key"
              autocomplete="one-time-code"
            />
          {/if}
          <button
            class="toggle-visibility"
            type="button"
            onclick={() => (showApiKey = !showApiKey)}
            aria-label={showApiKey ? 'Hide API key' : 'Show API key'}
          >
            <svg viewBox="0 0 24 24" width="16" height="16" fill="currentColor">
              {#if showApiKey}
                <path
                  d="M12 7c2.76 0 5 2.24 5 5 0 .65-.13 1.26-.36 1.83l2.92 2.92c1.51-1.26 2.7-2.89 3.43-4.75-1.73-4.39-6-7.5-11-7.5-1.4 0-2.74.25-3.98.7l2.16 2.16C10.74 7.13 11.35 7 12 7zM2 4.27l2.28 2.28.46.46A11.804 11.804 0 0 0 1 12c1.73 4.39 6 7.5 11 7.5 1.55 0 3.03-.3 4.38-.84l.42.42L19.73 22 21 20.73 3.27 3 2 4.27zM7.53 9.8l1.55 1.55c-.05.21-.08.43-.08.65 0 1.66 1.34 3 3 3 .22 0 .44-.03.65-.08l1.55 1.55c-.67.33-1.41.53-2.2.53-2.76 0-5-2.24-5-5 0-.79.2-1.53.53-2.2zm4.31-.78l3.15 3.15.02-.16c0-1.66-1.34-3-3-3l-.17.01z"
                ></path>
              {:else}
                <path
                  d="M12 4.5C7 4.5 2.73 7.61 1 12c1.73 4.39 6 7.5 11 7.5s9.27-3.11 11-7.5c-1.73-4.39-6-7.5-11-7.5zM12 17c-2.76 0-5-2.24-5-5s2.24-5 5-5 5 2.24 5 5-2.24 5-5 5zm0-8c-1.66 0-3 1.34-3 3s1.34 3 3 3 3-1.34 3-3-1.34-3-3-3z"
                ></path>
              {/if}
            </svg>
          </button>
        </div>
      </div>
      <label>
        Priority
        <input type="number" bind:value={form.priority} min="0" placeholder="0" />
      </label>
    </div>

    <div class="checkbox-row">
      <label class="checkbox">
        <input type="checkbox" bind:checked={form.enabled} />
        Enabled
      </label>
    </div>

    {#if testResult}
      <DebridTestResultView result={testResult} />
    {/if}
  </form>

  {#snippet footer()}
    <button
      class="btn compact alt"
      type="button"
      onclick={test}
      disabled={testing || !form.provider_type || !form.api_key.trim()}
    >
      {testing ? 'Testing...' : 'Test Connection'}
    </button>
    <button class="btn compact alt" type="button" onclick={cancel} disabled={saving}>Cancel</button>
    <button class="btn compact" type="submit" form={formId} disabled={saving}>
      {provider ? 'Update provider' : 'Add provider'}
    </button>
  {/snippet}
</Modal>

<style>
  .intro {
    margin: 0;
    font-size: 12px;
    color: var(--muted);
  }

  .api-key-wrapper {
    display: flex;
    position: relative;
  }

  .api-key-wrapper input {
    flex: 1;
    min-width: 0;
    padding-right: 40px;
  }

  .toggle-visibility {
    position: absolute;
    right: 4px;
    top: 50%;
    transform: translateY(-50%);
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 6px;
    background: transparent;
    border: none;
    border-radius: 6px;
    color: var(--muted);
    cursor: pointer;
  }

  .toggle-visibility:hover {
    color: var(--ink);
    background: rgba(255, 255, 255, 0.06);
  }

  :global([data-theme='light']) .toggle-visibility:hover {
    background: rgba(0, 0, 0, 0.06);
  }
</style>
