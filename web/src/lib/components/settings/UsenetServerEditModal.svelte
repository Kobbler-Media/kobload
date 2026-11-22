<script lang="ts">
  import { untrack } from 'svelte';
  import { Modal, SearchSelect, type SearchSelectItem } from '@koblibs/ui';
  import {
    createUsenetServer,
    updateUsenetServer,
    testUsenetServer,
    testUsenetServerById
  } from '#lib/api/client.js';
  import type {
    IpPreference,
    UsenetServer,
    UsenetServerInput,
    UsenetTestResult
  } from '#lib/types.js';
  import { discardGuard } from '#lib/discardGuard.js';

  let {
    server,
    onClose,
    onSaved
  }: {
    server: UsenetServer | null;
    onClose: () => void;
    onSaved: () => void;
  } = $props();

  const formId = 'usenet-server-edit-form';
  const TLS_PORT = 563;
  const PLAIN_PORT = 119;
  const BYTES_PER_GB = 1_000_000_000;

  const ipPreferences: SearchSelectItem[] = [
    { value: 'any', label: 'Any' },
    { value: 'v4', label: 'IPv4' },
    { value: 'v6', label: 'IPv6' }
  ];

  let form = $state(
    untrack(() => ({
      name: server?.name ?? '',
      host: server?.host ?? '',
      port: server?.port ?? TLS_PORT,
      tls: server?.tls ?? true,
      verify_cert: server?.verify_cert ?? true,
      ip_preference: server?.ip_preference ?? ('any' as IpPreference),
      connections: server?.connections ?? 20,
      priority: server?.priority ?? 0,
      backbone: server?.backbone ?? '',
      retention_days: server?.retention_days ?? null,
      expires_on: server?.expires_on ?? '',
      quota_gb: server?.quota_bytes != null ? server.quota_bytes / BYTES_PER_GB : null,
      username: server?.username ?? '',
      password: '',
      account_connection_cap: server?.account_connection_cap ?? null,
      enabled: server?.enabled ?? true
    }))
  );

  let saving = $state(false);
  let formError = $state('');
  let testing = $state(false);
  let testResult = $state<UsenetTestResult | null>(null);

  const canClose = discardGuard(form);
  const cancel = () => {
    if (canClose()) onClose();
  };

  const optionalNumber = (v: number | null | undefined): number | null =>
    v == null || Number.isNaN(v) ? null : Number(v);

  const optionalText = (v: string): string | null => v.trim() || null;

  const setTls = (tls: boolean) => {
    form.tls = tls;
    if (!tls && form.port === TLS_PORT) form.port = PLAIN_PORT;
    else if (tls && form.port === PLAIN_PORT) form.port = TLS_PORT;
  };

  const buildPayload = (): UsenetServerInput => {
    const quotaGb = optionalNumber(form.quota_gb);
    return {
      name: form.name.trim(),
      host: form.host.trim(),
      port: Number(form.port),
      tls: form.tls,
      verify_cert: form.verify_cert,
      ip_preference: form.ip_preference,
      connections: Number(form.connections),
      priority: Number(form.priority ?? 0),
      backbone: optionalText(form.backbone),
      retention_days: optionalNumber(form.retention_days),
      expires_on: optionalText(form.expires_on),
      quota_bytes: quotaGb == null ? null : Math.round(quotaGb * BYTES_PER_GB),
      username: form.username.trim(),
      password: form.password || null,
      account_connection_cap: optionalNumber(form.account_connection_cap),
      enabled: form.enabled
    };
  };

  const validate = (): string => {
    if (!form.name.trim() || !form.host.trim()) return 'Name and host are required.';
    const port = Number(form.port);
    if (!Number.isInteger(port) || port < 1 || port > 65535) return 'Port must be 1–65535.';
    if (!Number.isInteger(Number(form.connections)) || Number(form.connections) < 1)
      return 'Connections must be at least 1.';
    return '';
  };

  const save = async () => {
    formError = validate();
    if (formError) return;
    saving = true;
    try {
      const payload = buildPayload();
      if (server) {
        await updateUsenetServer(server.id, payload);
      } else {
        await createUsenetServer(payload);
      }
      onSaved();
    } catch (err) {
      formError = err instanceof Error ? err.message : 'Failed to save usenet server';
    } finally {
      saving = false;
    }
  };

  const test = async () => {
    formError = validate();
    if (formError) return;
    testing = true;
    testResult = null;
    try {
      testResult =
        server && !form.password
          ? await testUsenetServerById(server.id)
          : await testUsenetServer(buildPayload());
    } catch (err) {
      testResult = { success: false, message: err instanceof Error ? err.message : 'Test failed' };
    } finally {
      testing = false;
    }
  };
</script>

<Modal title={server ? `Edit ${server.name}` : 'Add Usenet Server'} sheet {onClose} {canClose}>
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
        Name
        <input type="text" bind:value={form.name} placeholder="My provider" />
      </label>
      <label>
        Host
        <input type="text" bind:value={form.host} placeholder="news.example.com" />
      </label>
      <label>
        Port
        <input type="number" bind:value={form.port} min="1" max="65535" />
      </label>
      <label>
        IP preference
        <SearchSelect
          id="ip-preference"
          items={ipPreferences}
          value={form.ip_preference}
          onchange={(v) => {
            form.ip_preference = v as IpPreference;
          }}
          placeholder="Any"
        />
      </label>
    </div>

    <div class="checkbox-row">
      <label class="checkbox">
        <input
          type="checkbox"
          checked={form.tls}
          onchange={(e) => setTls(e.currentTarget.checked)}
        />
        TLS
      </label>
      <label class="checkbox">
        <input type="checkbox" bind:checked={form.verify_cert} disabled={!form.tls} />
        Verify certificate
      </label>
    </div>

    <div class="form-grid">
      <label>
        Username
        <input type="text" bind:value={form.username} autocomplete="off" />
      </label>
      <label>
        Password
        <input
          type="password"
          bind:value={form.password}
          placeholder={server?.has_password ? 'unchanged' : ''}
          autocomplete="new-password"
        />
      </label>
      <label>
        Connections
        <input type="number" bind:value={form.connections} min="1" />
      </label>
      <label>
        Account connection cap
        <input
          type="number"
          bind:value={form.account_connection_cap}
          min="1"
          placeholder="Unknown"
        />
        <span class="hint">
          Total connections the provider allows for this login, summed over every server with the
          same host and username — saving over it is refused.
        </span>
      </label>
      <label>
        Priority tier
        <input type="number" bind:value={form.priority} min="0" />
        <span class="hint">
          Lower tiers are tried first; higher tiers are backups only used for articles every lower
          tier is missing.
        </span>
      </label>
      <label>
        Backbone group
        <input type="text" bind:value={form.backbone} placeholder="Optional" />
        <span class="hint">Servers on the same backbone share missing-article memory.</span>
      </label>
      <label>
        Retention (days)
        <input type="number" bind:value={form.retention_days} min="1" placeholder="Optional" />
      </label>
      <label>
        Account expiry
        <input type="date" bind:value={form.expires_on} />
      </label>
      <label>
        Block quota (GB)
        <input
          type="number"
          bind:value={form.quota_gb}
          min="0"
          step="any"
          placeholder="Unlimited"
        />
      </label>
    </div>

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
    <button class="btn compact alt" type="button" onclick={test} disabled={testing}>
      {testing ? 'Testing...' : 'Test Connection'}
    </button>
    <button class="btn compact alt" type="button" onclick={cancel} disabled={saving}>Cancel</button>
    <button class="btn compact" type="submit" form={formId} disabled={saving}>
      {server ? 'Update server' : 'Add server'}
    </button>
  {/snippet}
</Modal>

<style>
  .test-result {
    padding: 10px 14px;
    border-radius: 10px;
    font-size: 13px;
    background: rgba(220, 53, 69, 0.15);
    border: 1px solid rgba(220, 53, 69, 0.3);
    color: #dc3545;
  }

  .test-result.success {
    background: rgba(40, 167, 69, 0.15);
    border-color: rgba(40, 167, 69, 0.3);
    color: #28a745;
  }
</style>
