<script lang="ts">
  import { formatBytes } from '@koblibs/ui';
  import type { UsenetJobActivity } from '#lib/types.js';
  import { formatEta, formatSpeed, phaseLabels, waitLabels } from './usenetFormat.js';

  let { job }: { job: UsenetJobActivity } = $props();
</script>

<div class="job-stats">
  <span class="phase">{phaseLabels[job.phase]}</span>
  {#if job.total_bytes > 0}
    <span>{formatBytes(job.downloaded_bytes)} / {formatBytes(job.total_bytes)}</span>
  {/if}
  {#if job.waiting}
    <span class="waiting">{waitLabels[job.waiting]}</span>
  {:else if job.phase === 'downloading'}
    <span>{formatSpeed(job.speed_bps)}</span>
    {#if job.eta_seconds !== null}
      <span>ETA {formatEta(job.eta_seconds)}</span>
    {/if}
  {/if}
</div>

<style>
  .job-stats {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 10px;
    margin-top: 2px;
    font-size: 12px;
    color: var(--muted);
    font-variant-numeric: tabular-nums;
  }

  .phase {
    color: var(--ink);
    font-weight: 500;
  }

  .waiting {
    color: var(--accent);
    font-weight: 500;
  }
</style>
