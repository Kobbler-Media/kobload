<script lang="ts">
  import { untrack } from 'svelte';
  import { Modal, SearchSelect, type SearchSelectItem } from '@koblibs/ui';
  import {
    createIndexerSource,
    updateIndexerSource,
    testProwlarrConnection,
    testIndexerConnection,
    type TestConnectionResult,
    type DefinitionSummary,
    type IndexerHandlerInfo,
    type MediaType
  } from '$lib/api/client';
  import { discardGuard } from '$lib/discardGuard';
  import type { IndexerSource } from '$lib/types';

  let {
    source,
    definitions,
    handlers,
    mediaTypes,
    onClose,
    onSaved
  }: {
    source: IndexerSource | null;
    definitions: DefinitionSummary[];
    handlers: IndexerHandlerInfo[];
    mediaTypes: MediaType[];
    onClose: () => void;
    onSaved: () => void;
  } = $props();

  const FIXED_HANDLERS = ['prowlarr', 'newznab', 'torznab', 'cardigann'];
  const TESTABLE_HANDLERS = ['prowlarr', 'mam', 'torrentday'];
  const formId = 'indexer-edit-form';

  const buildForm = (src: IndexerSource | null) => {
    const config = (src?.handler_config ?? {}) as Record<string, unknown>;
    const dynamicConfig: Record<string, string> = {};
    if (src && !FIXED_HANDLERS.includes(src.handler_type)) {
      for (const [k, v] of Object.entries(config)) {
        dynamicConfig[k] = String(v ?? '');
      }
    }
    return {
      name: src?.name ?? '',
      handler_type: src?.handler_type ?? 'prowlarr',
      enabled: src?.enabled ?? true,
      base_url: (config.base_url as string) ?? '',
      api_key: (config.api_key as string) ?? '',
      definition: (config.definition as string) ?? '',
      settings: (config.settings as Record<string, string>) ?? {},
      poll_interval_minutes: String(config.poll_interval_minutes ?? 15),
      dynamic_config: dynamicConfig,
      media_type_ids: src
        ? ((src as unknown as { media_type_ids?: string[] }).media_type_ids ?? [])
        : [],
      category_ids: src?.category_ids?.join(', ') ?? '',
      priority: String(src?.priority ?? 0),
      seed_ratio: src?.seed_ratio != null ? String(src.seed_ratio) : '',
      seed_time_minutes: src?.seed_time_minutes != null ? String(src.seed_time_minutes) : '',
      search_mode: src?.search_mode ?? 'full',
      debrid_mode: src?.debrid_mode ?? 'never',
      auto_throttle: src?.auto_throttle ?? true,
      api_limit_daily: src?.api_limit_daily != null ? String(src.api_limit_daily) : ''
    };
  };

  let form = $state(untrack(() => buildForm(source)));
  let saving = $state(false);
  let formError = $state('');
  let testing = $state(false);
  let testResult = $state<TestConnectionResult | null>(null);

  const canClose = discardGuard(form);
  const cancel = () => {
    if (canClose()) onClose();
  };

  // Fallback list keeps the type picker usable if /indexer-handlers is unavailable.
  const FALLBACK_HANDLERS: SearchSelectItem[] = [
    { value: 'prowlarr', label: 'Prowlarr' },
    { value: 'newznab', label: 'Newznab' },
    { value: 'torznab', label: 'Torznab' },
    { value: 'cardigann', label: 'Cardigann' },
    { value: 'mam', label: 'MyAnonaMouse' },
    { value: 'torrentday', label: 'TorrentDay' }
  ];

  const handlerTypeItems = $derived<SearchSelectItem[]>(
    handlers.length > 0 ? handlers.map((h) => ({ value: h.id, label: h.name })) : FALLBACK_HANDLERS
  );

  const definitionItems = $derived<SearchSelectItem[]>(
    definitions.map((d) => ({
      value: d.name,
      label: d.name,
      description: d.description || undefined
    }))
  );

  const searchModeItems: SearchSelectItem[] = [
    { value: 'full', label: 'Full' },
    { value: 'rss_only', label: 'RSS only' },
    { value: 'backlog_only', label: 'Backlog only' }
  ];

  const debridModeItems: SearchSelectItem[] = [
    { value: 'never', label: 'Never' },
    { value: 'always', label: 'Debrid only' },
    { value: 'both', label: 'Both' }
  ];

  const selectedDef = $derived(
    form.handler_type === 'cardigann' && form.definition
      ? definitions.find((d) => d.name === form.definition)
      : null
  );

  const definitionInfo = $derived(
    selectedDef?.settings.filter((s) => s.setting_type.startsWith('info')) ?? []
  );

  const definitionSettings = $derived(
    selectedDef?.settings.filter((s) => !s.setting_type.startsWith('info')) ?? []
  );

  const currentHandlerSchema = $derived(
    handlers.find((h) => h.id === form.handler_type)?.config_schema ?? null
  );

  const usesDynamicConfig = $derived(
    !!currentHandlerSchema?.fields?.length && !FIXED_HANDLERS.includes(form.handler_type)
  );

  let lastDefinition = untrack(() => form.definition);
  $effect(() => {
    if (form.definition && form.definition !== lastDefinition) {
      lastDefinition = form.definition;
      const def = definitions.find((d) => d.name === form.definition);
      if (!def) return;
      if (!form.name.trim()) form.name = def.name;
      const next: Record<string, string> = { ...form.settings };
      for (const s of def.settings) {
        if (s.setting_type.startsWith('info')) continue;
        if (!(s.name in next)) next[s.name] = s.default ?? '';
      }
      form.settings = next;
    }
  });

  let lastHandlerType = untrack(() => form.handler_type);
  $effect(() => {
    if (form.handler_type !== lastHandlerType) {
      lastHandlerType = form.handler_type;
      testResult = null;
      if (usesDynamicConfig && currentHandlerSchema?.fields) {
        const next: Record<string, string> = { ...form.dynamic_config };
        for (const field of currentHandlerSchema.fields) {
          if (!(field.name in next) && field.default !== undefined) {
            next[field.name] = String(field.default);
          }
        }
        form.dynamic_config = next;
      }
    }
  });

  const buildHandlerConfig = (): Record<string, unknown> => {
    if (usesDynamicConfig) return { ...form.dynamic_config };
    switch (form.handler_type) {
      case 'prowlarr':
        return { base_url: form.base_url.trim(), api_key: form.api_key.trim() };
      case 'newznab':
      case 'torznab':
        return {
          base_url: form.base_url.trim(),
          api_key: form.api_key.trim() || undefined,
          poll_interval_minutes: parseInt(form.poll_interval_minutes) || 15
        };
      case 'cardigann':
        return {
          definition: form.definition,
          settings: form.settings,
          poll_interval_minutes: parseInt(form.poll_interval_minutes) || 15
        };
      default:
        return {};
    }
  };

  const save = async () => {
    if (!form.name.trim()) {
      formError = 'Name is required.';
      return;
    }
    saving = true;
    formError = '';
    try {
      const catIds = form.category_ids
        .split(',')
        .map((s) => parseInt(s.trim()))
        .filter((n) => !isNaN(n));

      const payload = {
        name: form.name.trim(),
        handler_type: form.handler_type,
        handler_config: buildHandlerConfig(),
        category_ids: catIds.length > 0 ? catIds : null,
        media_type_ids: form.media_type_ids.length > 0 ? form.media_type_ids : undefined,
        enabled: form.enabled,
        priority: form.priority ? parseInt(form.priority) : 0,
        seed_ratio: form.seed_ratio ? parseFloat(form.seed_ratio) : null,
        seed_time_minutes: form.seed_time_minutes ? parseInt(form.seed_time_minutes) : null,
        search_mode: form.search_mode,
        debrid_mode: form.debrid_mode,
        auto_throttle: form.auto_throttle,
        api_limit_daily: form.api_limit_daily ? parseInt(form.api_limit_daily) : null
      };
      if (source) {
        await updateIndexerSource(source.id, payload);
      } else {
        await createIndexerSource(payload);
      }
      onSaved();
    } catch (err) {
      formError = err instanceof Error ? err.message : 'Failed to save';
    } finally {
      saving = false;
    }
  };

  const test = async () => {
    testing = true;
    testResult = null;
    try {
      testResult =
        form.handler_type === 'prowlarr'
          ? await testProwlarrConnection({ base_url: form.base_url, api_key: form.api_key })
          : await testIndexerConnection(form.handler_type, buildHandlerConfig());
    } catch (err) {
      testResult = { success: false, message: err instanceof Error ? err.message : 'Test failed' };
    } finally {
      testing = false;
    }
  };
</script>

{#snippet fieldHelp(help: string, helpUrl?: string)}
  <span class="hint">
    {help}
    {#if helpUrl}
      <a href={helpUrl} target="_blank" rel="noreferrer noopener">Open</a>
    {/if}
  </span>
{/snippet}

<Modal
  title={source ? `Edit ${source.name}` : 'Add Indexer'}
  width="760px"
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

    <div class="form-grid">
      <label>
        Type
        <SearchSelect
          id="handler-type"
          items={handlerTypeItems}
          value={form.handler_type}
          onchange={(v) => {
            form.handler_type = v;
          }}
          placeholder="Select type..."
        />
      </label>
      <label>
        Name
        <input type="text" bind:value={form.name} placeholder="My Indexer" />
      </label>

      {#if form.handler_type === 'prowlarr' || form.handler_type === 'newznab' || form.handler_type === 'torznab'}
        <label>
          Base URL
          <input
            type="text"
            bind:value={form.base_url}
            placeholder={form.handler_type === 'prowlarr'
              ? 'http://prowlarr:9696'
              : 'https://indexer.example.com'}
          />
        </label>
        <label>
          API key
          <input
            type="password"
            bind:value={form.api_key}
            placeholder="API key"
            autocomplete="one-time-code"
          />
        </label>
      {/if}

      {#if form.handler_type === 'cardigann'}
        <label>
          Definition
          <SearchSelect
            items={definitionItems}
            bind:value={form.definition}
            placeholder="Search definitions..."
          />
        </label>
      {/if}

      {#if form.handler_type !== 'prowlarr' && !usesDynamicConfig}
        <label>
          Poll interval (min)
          <input type="number" bind:value={form.poll_interval_minutes} min="1" />
        </label>
      {/if}
    </div>

    {#if usesDynamicConfig && currentHandlerSchema?.fields}
      <div class="form-grid">
        {#each currentHandlerSchema.fields as field (field.name)}
          {#if field.type === 'checkbox'}
            <div class="field">
              <label class="checkbox">
                <input
                  type="checkbox"
                  checked={form.dynamic_config[field.name] === 'true'}
                  onchange={(e) =>
                    (form.dynamic_config = {
                      ...form.dynamic_config,
                      [field.name]: e.currentTarget.checked ? 'true' : 'false'
                    })}
                />
                {field.label}
              </label>
              {#if field.help}
                {@render fieldHelp(field.help, field.help_url)}
              {/if}
            </div>
          {:else}
            <label>
              {field.label}
              {#if field.type === 'select' && field.options}
                <SearchSelect
                  id="dynamic-{field.name}"
                  items={Object.entries(field.options).map(([v, l]) => ({
                    value: v,
                    label: String(l)
                  }))}
                  value={form.dynamic_config[field.name] ?? ''}
                  onchange={(v) => {
                    form.dynamic_config = { ...form.dynamic_config, [field.name]: v };
                  }}
                  placeholder="Select..."
                />
              {:else}
                <input
                  type={field.type === 'password' ? 'password' : 'text'}
                  value={form.dynamic_config[field.name] ?? ''}
                  oninput={(e) =>
                    (form.dynamic_config = {
                      ...form.dynamic_config,
                      [field.name]: e.currentTarget.value
                    })}
                  placeholder={field.placeholder ?? ''}
                  autocomplete={field.type === 'password' ? 'one-time-code' : 'off'}
                />
              {/if}
              {#if field.help}
                {@render fieldHelp(field.help, field.help_url)}
              {/if}
            </label>
          {/if}
        {/each}
      </div>
    {/if}

    {#each definitionInfo as setting (setting.name)}
      <div class="info-banner">
        {#if setting.label}<strong>{setting.label}</strong>{/if}
        {#if setting.default}
          <p>{setting.default}</p>
        {:else if setting.info_description}
          <p>{setting.info_description}</p>
        {/if}
      </div>
    {/each}

    {#if definitionSettings.length > 0}
      <div class="form-grid">
        {#each definitionSettings as setting (setting.name)}
          {#if setting.setting_type === 'checkbox'}
            <label class="checkbox">
              <input
                type="checkbox"
                checked={form.settings[setting.name] === 'true'}
                onchange={(e) =>
                  (form.settings = {
                    ...form.settings,
                    [setting.name]: e.currentTarget.checked ? 'true' : 'false'
                  })}
              />
              {setting.label ?? setting.name}
            </label>
          {:else}
            <label>
              {setting.label ?? setting.name}
              {#if setting.setting_type === 'select' && setting.options}
                <SearchSelect
                  id="setting-{setting.name}"
                  items={Object.entries(setting.options).map(([v, l]) => ({
                    value: v,
                    label: String(l)
                  }))}
                  value={form.settings[setting.name] ?? ''}
                  onchange={(v) => {
                    form.settings = { ...form.settings, [setting.name]: v };
                  }}
                  placeholder="Select..."
                />
              {:else}
                <input
                  type={setting.setting_type === 'password' ? 'password' : 'text'}
                  value={form.settings[setting.name] ?? ''}
                  oninput={(e) =>
                    (form.settings = { ...form.settings, [setting.name]: e.currentTarget.value })}
                  autocomplete={setting.setting_type === 'password' ? 'one-time-code' : 'off'}
                />
              {/if}
            </label>
          {/if}
        {/each}
      </div>
    {/if}

    <details class="advanced-section">
      <summary>Advanced</summary>
      <div class="form-grid">
        <div class="field">
          <span>Media types</span>
          <div class="checkbox-group">
            {#each mediaTypes as mt (mt.id)}
              <label class="checkbox">
                <input
                  type="checkbox"
                  checked={form.media_type_ids.includes(mt.id)}
                  onchange={(e) => {
                    form.media_type_ids = e.currentTarget.checked
                      ? [...form.media_type_ids, mt.id]
                      : form.media_type_ids.filter((id) => id !== mt.id);
                  }}
                />
                {mt.name}
              </label>
            {/each}
          </div>
          <span class="hint">Leave empty to serve all media types</span>
        </div>
        <label>
          Category IDs
          <input type="text" bind:value={form.category_ids} placeholder="7020, 7030, 3030" />
          <span class="hint">Comma-separated newznab category IDs</span>
        </label>
        <label>
          Priority
          <input type="number" bind:value={form.priority} placeholder="0" />
          <span class="hint">Higher = searched first</span>
        </label>
        <label>
          Seed ratio override
          <input
            type="number"
            bind:value={form.seed_ratio}
            step="0.1"
            min="0"
            placeholder="Client default"
          />
        </label>
        <label>
          Seed time override (min)
          <input
            type="number"
            bind:value={form.seed_time_minutes}
            min="0"
            placeholder="Client default"
          />
        </label>
        <label>
          Search mode
          <SearchSelect
            id="search-mode"
            items={searchModeItems}
            value={form.search_mode}
            onchange={(v) => {
              form.search_mode = v;
            }}
            placeholder="Full"
          />
          <span class="hint">Full = RSS + interactive + backlog</span>
        </label>
        <label>
          Debrid mode
          <SearchSelect
            id="debrid-mode"
            items={debridModeItems}
            value={form.debrid_mode}
            onchange={(v) => {
              form.debrid_mode = v;
            }}
            placeholder="Never"
          />
          <span class="hint">Debrid only = stream search only</span>
        </label>
        <label>
          Daily API limit
          <input
            type="number"
            bind:value={form.api_limit_daily}
            min="0"
            placeholder="Auto-detect"
          />
          <span class="hint">0 or empty = unlimited</span>
        </label>
        <label class="checkbox">
          <input type="checkbox" bind:checked={form.auto_throttle} />
          Auto-throttle backlog at 90%
        </label>
      </div>
    </details>

    <div class="checkbox-row">
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
    {#if TESTABLE_HANDLERS.includes(form.handler_type)}
      <button class="btn compact alt" type="button" onclick={test} disabled={testing}>
        {testing ? 'Testing...' : 'Test'}
      </button>
    {/if}
    <button class="btn compact alt" type="button" onclick={cancel} disabled={saving}>Cancel</button>
    <button class="btn compact" type="submit" form={formId} disabled={saving}>
      {source ? 'Update' : 'Add indexer'}
    </button>
  {/snippet}
</Modal>
