<script lang="ts">
  import { untrack } from 'svelte';
  import { Modal, SearchSelect, type SearchSelectItem } from '@koblibs/ui';
  import {
    createDownloadClient,
    updateDownloadClient,
    testDownloadClientConfig,
    type DownloadClientConfig,
    type DownloadClientTypeInfo,
    type TestConnectionResult
  } from '#lib/api/client.js';
  import { discardGuard } from '#lib/discardGuard.js';

  let {
    client,
    clientTypes,
    existingClients,
    onClose,
    onSaved
  }: {
    client: DownloadClientConfig | null;
    clientTypes: DownloadClientTypeInfo[];
    existingClients: DownloadClientConfig[];
    onClose: () => void;
    onSaved: () => void;
  } = $props();

  const formId = 'download-client-edit-form';

  // New clients follow the picked type: named after it, and default for that
  // protocol when nothing else is. Applied to the initial value as well as on
  // change, so an untouched Add dialog still counts as pristine.
  const typeDefaults = (typeId: string) => {
    const info = clientTypes.find((t) => t.id === typeId);
    if (!info) return null;
    return {
      name: info.name,
      is_default: !existingClients.some((c) => c.protocol === info.protocol && c.is_default)
    };
  };

  let form = $state(
    untrack(() => {
      const seed = client ? null : typeDefaults('transmission');
      return {
        name: client?.name ?? seed?.name ?? '',
        client_type: client?.client_type ?? 'transmission',
        base_url: client?.base_url ?? '',
        api_key: client?.api_key ?? '',
        username: client?.username ?? '',
        password: client?.password ?? '',
        category: client?.category ?? '',
        download_dir: client?.download_dir ?? '',
        is_default: client?.is_default ?? seed?.is_default ?? false,
        delete_after_import: client?.delete_after_import ?? false,
        seed_ratio: client?.seed_ratio?.toString() ?? '',
        seed_time_minutes: client?.seed_time_minutes?.toString() ?? '',
        enabled: client?.enabled ?? true
      };
    })
  );

  let saving = $state(false);
  let formError = $state('');
  let testing = $state(false);
  let testResult = $state<TestConnectionResult | null>(null);

  const canClose = discardGuard(form);
  const cancel = () => {
    if (canClose()) onClose();
  };

  const clientTypeItems = $derived<SearchSelectItem[]>(
    clientTypes.map((t) => ({ value: t.id, label: `${t.name} (${t.protocol})` }))
  );

  const selectedType = $derived(clientTypes.find((t) => t.id === form.client_type));

  $effect(() => {
    const seed = client ? null : typeDefaults(form.client_type);
    if (!seed) return;

    const allTypeNames = clientTypes.map((t) => t.name);
    if (!form.name || allTypeNames.includes(form.name)) {
      form.name = seed.name;
    }
    if (seed.is_default) {
      form.is_default = true;
    }
  });

  const save = async () => {
    if (!form.name.trim() || !form.base_url.trim()) {
      formError = 'Name and base URL are required.';
      return;
    }
    saving = true;
    formError = '';
    try {
      const payload = {
        name: form.name.trim(),
        client_type: form.client_type,
        protocol: selectedType?.protocol ?? 'torrent',
        base_url: form.base_url.trim(),
        api_key: form.api_key.trim() || null,
        username: form.username.trim() || null,
        password: form.password.trim() || null,
        category: form.category.trim() || null,
        download_dir: form.download_dir.trim() || null,
        is_default: form.is_default,
        delete_after_import: form.delete_after_import,
        seed_ratio: form.seed_ratio ? Number.parseFloat(form.seed_ratio) : null,
        seed_time_minutes: form.seed_time_minutes
          ? Number.parseInt(form.seed_time_minutes, 10)
          : null,
        enabled: form.enabled
      };
      if (client) {
        await updateDownloadClient(client.id, payload);
      } else {
        await createDownloadClient(payload);
      }
      onSaved();
    } catch (err) {
      formError = err instanceof Error ? err.message : 'Failed to save download client';
    } finally {
      saving = false;
    }
  };

  const test = async () => {
    if (!selectedType) {
      testResult = { success: false, message: 'Select a client type' };
      return;
    }
    const config: { client_type: string } & Record<string, unknown> = {
      client_type: form.client_type
    };
    for (const field of selectedType.fields) {
      const value = form[field.name as keyof typeof form];
      if (value) config[field.name] = value;
    }
    testing = true;
    testResult = null;
    try {
      testResult = await testDownloadClientConfig(config);
    } catch (err) {
      testResult = { success: false, message: err instanceof Error ? err.message : 'Test failed' };
    } finally {
      testing = false;
    }
  };
</script>

<Modal title={client ? `Edit ${client.name}` : 'Add Download Client'} sheet {onClose} {canClose}>
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

    <div class="form-grid">
      <label>
        Client type
        <SearchSelect
          id="client-type"
          items={clientTypeItems}
          value={form.client_type}
          onchange={(v) => {
            form.client_type = v;
            testResult = null;
          }}
          placeholder="Select client type..."
        />
      </label>
      <label>
        Name
        <input type="text" bind:value={form.name} placeholder="My Download Client" />
      </label>
      {#each selectedType?.fields ?? [] as field (field.name)}
        {#if field.field_type === 'checkbox'}
          <label class="checkbox">
            <input
              type="checkbox"
              checked={form[field.name as keyof typeof form] as boolean}
              onchange={(e) => {
                (form as Record<string, unknown>)[field.name] = e.currentTarget.checked;
              }}
            />
            {field.label}
          </label>
        {:else}
          <label>
            {field.label}
            <input
              type={field.field_type === 'password' ? 'password' : 'text'}
              bind:value={form[field.name as keyof typeof form]}
              placeholder={field.placeholder ?? ''}
              autocomplete={field.field_type === 'password' ? 'one-time-code' : 'off'}
            />
          </label>
        {/if}
      {/each}
    </div>

    <div class="checkbox-row">
      <label class="checkbox">
        <input type="checkbox" bind:checked={form.is_default} />
        Default
      </label>
      <label class="checkbox">
        <input type="checkbox" bind:checked={form.delete_after_import} />
        Delete after import
      </label>
      <label class="checkbox">
        <input type="checkbox" bind:checked={form.enabled} />
        Enabled
      </label>
    </div>

    {#if testResult}
      <div class="test-result" class:success={testResult.success}>{testResult.message}</div>
    {/if}
  </form>

  {#snippet footer()}
    <button
      class="btn compact alt"
      type="button"
      onclick={test}
      disabled={testing || !form.client_type || !form.base_url}
    >
      {testing ? 'Testing...' : 'Test'}
    </button>
    <button class="btn compact alt" type="button" onclick={cancel} disabled={saving}>Cancel</button>
    <button class="btn compact" type="submit" form={formId} disabled={saving}>
      {client ? 'Update client' : 'Add client'}
    </button>
  {/snippet}
</Modal>
