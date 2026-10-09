<script lang="ts">
  import { onMount } from 'svelte';
  import { getDecisionLogSettings, updateDecisionLogSettings } from '$lib/api/client';
  import { showToast } from '$lib/stores/toast.svelte';

  let {
    type
  }: {
    type: 'search' | 'feed' | 'debrid';
  } = $props();

  let enabled = $state(false);
  let loading = $state(true);
  let toggling = $state(false);

  const label = $derived(
    type === 'search' ? 'Search Logging' : type === 'feed' ? 'Feed Logging' : 'Debrid Logging'
  );
  const settingsKey = $derived(
    type === 'search'
      ? 'search_logging_enabled'
      : type === 'feed'
        ? 'feed_logging_enabled'
        : 'debrid_logging_enabled'
  );

  onMount(async () => {
    try {
      const settings = await getDecisionLogSettings();
      enabled = settings[settingsKey];
    } catch {
      // Non-critical
    } finally {
      loading = false;
    }
  });

  async function handleToggle() {
    toggling = true;
    const newValue = !enabled;
    try {
      await updateDecisionLogSettings({ [settingsKey]: newValue });
      enabled = newValue;
      showToast(`${label} ${newValue ? 'enabled' : 'disabled'}`);
    } catch (err) {
      showToast(err instanceof Error ? err.message : 'Failed to update settings');
    } finally {
      toggling = false;
    }
  }
</script>

<button
  class="log-toggle"
  type="button"
  onclick={handleToggle}
  disabled={loading || toggling}
  title="{label}: {enabled ? 'On' : 'Off'}"
>
  <span class="toggle-label">{label}</span>
  <span class="toggle-switch" class:on={enabled}>
    <span class="toggle-thumb"></span>
  </span>
</button>

<style>
  .log-toggle {
    display: flex;
    align-items: center;
    gap: 8px;
    background: none;
    border: none;
    cursor: pointer;
    padding: 4px 0;
    color: var(--ink);
    font: inherit;
    font-size: 13px;
  }

  .log-toggle:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .toggle-label {
    color: var(--muted);
    font-size: 12px;
    white-space: nowrap;
  }

  .toggle-switch {
    position: relative;
    width: 36px;
    height: 20px;
    border-radius: 10px;
    background: var(--ring);
    transition: background 0.2s;
    flex-shrink: 0;
  }

  .toggle-switch.on {
    background: #22c55e;
  }

  .toggle-thumb {
    position: absolute;
    top: 2px;
    left: 2px;
    width: 16px;
    height: 16px;
    border-radius: 50%;
    background: white;
    transition: transform 0.2s;
  }

  .toggle-switch.on .toggle-thumb {
    transform: translateX(16px);
  }
</style>
