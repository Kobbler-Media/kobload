<script lang="ts">
  import { untrack } from 'svelte';
  import { Modal } from '@koblibs/ui';
  import { createWebhook, updateWebhook, type StatusWebhook } from '$lib/api/client';
  import { discardGuard } from '$lib/discardGuard';

  let {
    webhook,
    events,
    onClose,
    onSaved
  }: {
    webhook: StatusWebhook | null;
    events: string[];
    onClose: () => void;
    onSaved: () => void;
  } = $props();

  const formId = 'webhook-edit-form';

  let form = $state(
    untrack(() => ({
      url: webhook?.url ?? '',
      secret: '',
      events: [...(webhook?.events ?? [])],
      enabled: webhook?.enabled ?? true
    }))
  );
  let saving = $state(false);
  let formError = $state('');

  const canClose = discardGuard(form);
  const cancel = () => {
    if (canClose()) onClose();
  };

  const toggleEvent = (event: string) => {
    form.events = form.events.includes(event)
      ? form.events.filter((e) => e !== event)
      : [...form.events, event];
  };

  const save = async () => {
    saving = true;
    formError = '';
    try {
      if (webhook) {
        const data: Record<string, unknown> = {
          url: form.url,
          events: form.events,
          enabled: form.enabled
        };
        if (form.secret) data.secret = form.secret;
        await updateWebhook(webhook.id, data as Parameters<typeof updateWebhook>[1]);
      } else {
        await createWebhook({
          url: form.url,
          secret: form.secret || undefined,
          events: form.events
        });
      }
      onSaved();
    } catch (err) {
      formError = err instanceof Error ? err.message : 'Failed to save webhook';
    } finally {
      saving = false;
    }
  };
</script>

<Modal title={webhook ? 'Edit Webhook' : 'New Webhook'} sheet {onClose} {canClose}>
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

    <div class="form-grid cols-1">
      <label>
        URL
        <input
          type="url"
          bind:value={form.url}
          required
          placeholder="https://example.com/webhook"
        />
      </label>

      <label>
        Secret
        <input
          type="password"
          bind:value={form.secret}
          placeholder={webhook ? 'Leave blank to keep current' : 'Optional signing secret'}
          autocomplete="one-time-code"
        />
        <span class="hint">Used to sign payloads with HMAC-SHA256.</span>
      </label>

      <div class="field">
        <span>Events</span>
        <div class="event-chips">
          {#each events as event (event)}
            <button
              type="button"
              class="chip"
              class:active={form.events.includes(event)}
              onclick={() => toggleEvent(event)}
            >
              {event}
            </button>
          {/each}
        </div>
        <span class="hint">Select specific events or leave empty to receive all.</span>
      </div>

      {#if webhook}
        <label class="checkbox">
          <input type="checkbox" bind:checked={form.enabled} />
          Enabled
        </label>
      {/if}
    </div>
  </form>

  {#snippet footer()}
    <button class="btn compact alt" type="button" onclick={cancel} disabled={saving}>Cancel</button>
    <button class="btn compact" type="submit" form={formId} disabled={saving}>
      {saving ? 'Saving...' : webhook ? 'Update' : 'Create'}
    </button>
  {/snippet}
</Modal>

<style>
  .event-chips {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }

  .chip {
    padding: 4px 10px;
    font-size: 11px;
    border-radius: 6px;
    border: 1px solid var(--ring);
    background: transparent;
    color: var(--ink);
    cursor: pointer;
  }

  .chip.active {
    background: var(--accent);
    border-color: var(--accent);
    color: #1a1b14;
  }
</style>
