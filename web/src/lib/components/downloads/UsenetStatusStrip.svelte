<script lang="ts">
  import { onMount } from 'svelte';
  import { activityLabel, formatSpeed, streamLabels } from './usenetFormat.js';
  import { usenetStatus, watchUsenetStatus } from './usenetStatus.svelte.js';

  onMount(watchUsenetStatus);

  const response = $derived(usenetStatus());
  const status = $derived(response?.engine === 'available' ? response.status : null);
  const troubled = $derived(
    status?.activity.kind === 'provider_failing' || status?.activity.kind === 'paused_low_disk'
  );
</script>

{#if response?.engine === 'unavailable'}
  <section class="strip" aria-label="Usenet engine">
    <span class="state troubled">Usenet engine unavailable</span>
  </section>
{:else if status}
  <section class="strip" aria-label="Usenet engine">
    <span class="state" class:troubled>{activityLabel(status.activity)}</span>
    <span class="speed">{formatSpeed(status.speed_bps)}</span>
    {#each status.providers as provider, index (index)}
      <span class="provider" class:troubled={provider.failing}>
        {provider.host}
        <span class="connections">{provider.in_use} / {provider.open}</span>
      </span>
    {/each}
  </section>

  {#if status.streams.length > 0}
    <section class="streams" aria-label="Streaming now">
      <h2>Streaming now</h2>
      {#each status.streams as stream (stream.id)}
        <div class="stream-row">
          <span class="stream-name">{stream.name}</span>
          <span class="stream-meta">
            <span>{streamLabels[stream.kind]}</span>
            {#if stream.position_percent !== null}
              <span>{stream.position_percent}%</span>
            {/if}
            <span>{formatSpeed(stream.speed_bps)}</span>
          </span>
        </div>
      {/each}
    </section>
  {/if}
{/if}

<style>
  .strip,
  .streams {
    margin-bottom: 12px;
    background: var(--card);
    border: 1px solid var(--ring);
    border-radius: 12px;
  }

  .strip {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 16px;
    padding: 10px 14px;
    font-size: 13px;
    font-variant-numeric: tabular-nums;
  }

  .state {
    font-weight: 600;
  }

  .troubled {
    color: #dc3545;
  }

  .speed {
    font-weight: 500;
  }

  .provider {
    color: var(--muted);
    overflow-wrap: anywhere;
  }

  .connections {
    margin-left: 4px;
    color: var(--ink);
  }

  .streams {
    overflow: hidden;
  }

  h2 {
    margin: 0;
    padding: 8px 14px;
    font-size: 12px;
    font-weight: 600;
    color: var(--muted);
  }

  .stream-row {
    display: flex;
    flex-wrap: wrap;
    justify-content: space-between;
    gap: 2px 12px;
    padding: 8px 14px;
    border-top: 1px solid var(--ring);
    font-size: 13px;
  }

  .stream-name {
    min-width: 0;
    font-weight: 500;
    overflow-wrap: anywhere;
  }

  .stream-meta {
    display: flex;
    gap: 10px;
    font-size: 12px;
    color: var(--muted);
    font-variant-numeric: tabular-nums;
  }
</style>
