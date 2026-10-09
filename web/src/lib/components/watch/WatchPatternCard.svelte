<script lang="ts">
  import type { WatchPattern } from '$lib/types';
  import { getEffectiveStatus, getPatternTitle } from './grouping';
  import { getMediaKindIcon, getStatusIcon } from './mediaIcons';
  import { getWatchStatusStyle, getScoreColor } from '$lib/styles/statusColors';

  let {
    pattern,
    selected = false,
    selectable = false,
    onEdit,
    onSearch,
    onTest,
    onDelete,
    onToggleEnabled,
    onSelect,
    onViewLogs
  }: {
    pattern: WatchPattern;
    selected?: boolean;
    selectable?: boolean;
    onEdit: (p: WatchPattern) => void;
    onSearch: (p: WatchPattern) => void;
    onTest: (p: WatchPattern) => void;
    onDelete: (p: WatchPattern) => void;
    onToggleEnabled: (p: WatchPattern) => void;
    onSelect?: (p: WatchPattern) => void;
    onViewLogs?: (p: WatchPattern) => void;
  } = $props();

  const effectiveStatus = $derived(getEffectiveStatus(pattern));
  const statusStyle = $derived(getWatchStatusStyle(effectiveStatus));
  const title = $derived(getPatternTitle(pattern));
  const mediaKindSvg = $derived(getMediaKindIcon(pattern.media_want?.kind));
  const statusIconSvg = $derived(getStatusIcon(effectiveStatus));

  const year = $derived.by(() => {
    const mw = pattern.media_want;
    if (!mw) return null;
    if ('year' in mw && mw.year) return mw.year;
    return null;
  });

  const statusDetail = $derived.by(() => {
    if (!pattern.enabled) return 'disabled';
    if (
      pattern.episode_progress &&
      pattern.episode_progress.imported < pattern.episode_progress.total
    ) {
      return `${pattern.episode_progress.imported}/${pattern.episode_progress.total}`;
    }
    if (pattern.upgrade_allowed && pattern.upgrade_until) {
      const d = new Date(pattern.upgrade_until);
      if (d > new Date()) {
        return `cleanup ${d.toLocaleDateString('en-US', { month: 'short', day: 'numeric' })}`;
      }
      return 'cleanup pending';
    }
    if (pattern.upgrade_allowed) {
      return 'upgrades on';
    }
    return null;
  });

  const scoreColor = (score: number | null | undefined): string => {
    if (score == null) return 'var(--muted)';
    return getScoreColor(score);
  };
</script>

<article class="card">
  <div class="card-title-row">
    {#if selectable}
      <input
        type="checkbox"
        checked={selected}
        onchange={() => onSelect?.(pattern)}
        onclick={(e) => e.stopPropagation()}
      />
    {/if}
    <span class="status-icon" style="color: {statusStyle.color}" title={statusStyle.label}>
      <!-- eslint-disable-next-line svelte/no-at-html-tags -- trusted SVG from mediaIcons.ts -->
      {@html statusIconSvg}
    </span>
    {#if mediaKindSvg}
      <!-- eslint-disable-next-line svelte/no-at-html-tags -- trusted SVG from mediaIcons.ts -->
      <span class="media-icon">{@html mediaKindSvg}</span>
    {/if}
    <h3 class="card-title">
      {title}
      {#if year}
        <span class="year-suffix">({year})</span>
      {/if}
    </h3>
    <span class="source-label">{pattern.source_system}</span>
  </div>

  <div class="meta-line">
    <span class="meta-status" style="color: {statusStyle.color}">
      {statusStyle.label}
      {#if statusDetail}
        <span class="meta-detail">{statusDetail}</span>
      {/if}
    </span>
    <span class="meta-sep">·</span>
    <span>{pattern.match_mode}</span>
    <span class="meta-sep">·</span>
    <span>{pattern.auto_download ? 'auto' : 'manual'}</span>
    {#if pattern.min_score !== null}
      <span class="meta-sep">·</span>
      <span class="meta-score" style="color: {scoreColor(pattern.min_score)}">
        min {(pattern.min_score * 100).toFixed(0)}%
      </span>
    {/if}
    {#if pattern.best_quality_score != null}
      <span class="meta-sep">·</span>
      <span class="meta-score" style="color: {scoreColor(pattern.best_quality_score)}">
        best {(pattern.best_quality_score * 100).toFixed(0)}%
      </span>
    {/if}
  </div>

  <div class="actions">
    <button class="btn compact" type="button" onclick={() => onSearch(pattern)}>Search</button>
    <button class="btn compact alt" type="button" onclick={() => onTest(pattern)}>Test</button>
    {#if onViewLogs}
      <button class="btn compact alt" type="button" onclick={() => onViewLogs(pattern)}>Logs</button
      >
    {/if}
    <button class="btn compact alt" type="button" onclick={() => onEdit(pattern)}>Edit</button>
    <button class="btn compact ghost" type="button" onclick={() => onToggleEnabled(pattern)}>
      {pattern.enabled ? 'Disable' : 'Enable'}
    </button>
    <button class="btn compact ghost" type="button" onclick={() => onDelete(pattern)}>Delete</button
    >
  </div>
</article>

<style>
  .card {
    background: var(--card);
    border: 1px solid var(--ring);
    border-radius: 12px;
    padding: 12px 16px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .card-title-row {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }

  .status-icon {
    display: flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
    width: 16px;
    height: 16px;
    opacity: 0.9;
  }

  .media-icon {
    display: flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
    width: 16px;
    height: 16px;
    color: var(--muted);
    opacity: 0.7;
  }

  .card-title {
    margin: 0;
    font-size: 14px;
    font-weight: 600;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    line-height: 1.3;
    flex: 1;
    min-width: 0;
  }

  .year-suffix {
    font-weight: 400;
    color: var(--muted);
    font-size: 13px;
  }

  .source-label {
    font-size: 11px;
    color: var(--muted);
    flex-shrink: 0;
    opacity: 0.6;
  }

  .meta-line {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    align-items: center;
    font-size: 11px;
    color: var(--muted);
    padding-left: 24px;
  }

  .meta-status {
    font-weight: 600;
  }

  .meta-detail {
    font-weight: 400;
    opacity: 0.8;
  }

  .meta-score {
    font-weight: 500;
  }

  .meta-sep {
    opacity: 0.3;
  }

  .actions {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
    padding-top: 4px;
    border-top: 1px solid var(--ring);
  }

  .actions :global(.btn.ghost:hover:not(:disabled)) {
    color: #dc3545;
  }
</style>
