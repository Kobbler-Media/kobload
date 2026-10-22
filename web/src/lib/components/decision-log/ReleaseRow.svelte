<script lang="ts">
  import type { DecisionLogRelease } from '$lib/types';

  let {
    release,
    compareMode = false,
    compareSelected = false,
    onToggleCompare
  }: {
    release: DecisionLogRelease;
    compareMode?: boolean;
    compareSelected?: boolean;
    onToggleCompare?: (release: DecisionLogRelease) => void;
  } = $props();

  let expanded = $state(false);

  const decisionStyle = $derived(getDecisionStyle(release.decision));

  function getDecisionStyle(decision: string): { bg: string; color: string; label: string } {
    if (decision === 'best')
      return { bg: 'rgba(34, 197, 94, 0.15)', color: '#22c55e', label: 'Best' };
    if (decision === 'candidate')
      return { bg: 'rgba(234, 179, 8, 0.15)', color: '#eab308', label: 'Candidate' };
    if (decision.startsWith('rejected')) {
      const reason = decision.replace('rejected_', '').replace(/_/g, ' ');
      return { bg: 'rgba(239, 68, 68, 0.15)', color: '#ef4444', label: reason || 'Rejected' };
    }
    return { bg: 'rgba(156, 163, 175, 0.15)', color: '#9ca3af', label: decision };
  }

  function formatScore(score: number | null): string {
    if (score === null) return '-';
    return `${(score * 100).toFixed(0)}%`;
  }

  function formatBytes(bytes: number): string {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
    return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
  }

  /** Human-readable one-liner explaining the decision. */
  const whySummary = $derived.by(() => {
    const d = release.decision;
    const pq = release.parsed_quality as Record<string, unknown> | null;
    const sr = release.structural_result;
    const bd = release.quality_score_breakdown;

    if (d === 'best') {
      const parts: string[] = [];
      if (release.matched_pattern) parts.push(`matched "${release.matched_pattern}"`);
      if (release.match_score !== null)
        parts.push(`${(release.match_score * 100).toFixed(0)}% match`);
      if (release.quality_score !== null) parts.push(`quality ${release.quality_score.toFixed(2)}`);
      return `Winner: ${parts.join(', ')}`;
    }
    if (d === 'candidate') {
      const parts: string[] = [];
      if (release.match_score !== null)
        parts.push(`${(release.match_score * 100).toFixed(0)}% match`);
      if (release.quality_score !== null) parts.push(`quality ${release.quality_score.toFixed(2)}`);
      return `Candidate: ${parts.join(', ')}`;
    }
    if (d === 'rejected_quality' && bd) {
      const reason = bd.rejected_reason ?? 'quality check';
      const res = pq?.resolution ?? 'unknown';
      return `Rejected: ${reason.replace(/_/g, ' ')} (${res})`;
    }
    if (d === 'rejected_structural' && sr) {
      const reason = sr.reject_reason ?? 'structural mismatch';
      const detail: string[] = [];
      if (sr.parsed_season !== null && sr.parsed_season !== undefined)
        detail.push(`parsed S${String(sr.parsed_season).padStart(2, '0')}`);
      if (sr.want_season !== null && sr.want_season !== undefined)
        detail.push(`want S${String(sr.want_season).padStart(2, '0')}`);
      return `Rejected: ${reason.replace(/_/g, ' ')}${detail.length ? ` (${detail.join(' vs ')})` : ''}`;
    }
    if (d === 'rejected_match') {
      const score =
        release.match_score !== null ? ` (${(release.match_score * 100).toFixed(0)}%)` : '';
      return `Rejected: below match threshold${score}`;
    }
    if (d === 'rejected_trash') {
      return `Rejected: TRaSH filter${bd?.rejected_reason ? ` (${bd.rejected_reason})` : ''}`;
    }
    if (d === 'rejected_duplicate') return 'Rejected: duplicate release';
    if (d === 'rejected_category') return 'Rejected: category mismatch';
    if (d === 'rejected_excluded') return 'Rejected: in exclusion list';
    if (d === 'rejected_upgrade') return 'Rejected: not a quality upgrade';
    if (d.startsWith('rejected_')) {
      return `Rejected: ${d.replace('rejected_', '').replace(/_/g, ' ')}${release.decision_reason ? ` — ${release.decision_reason}` : ''}`;
    }
    return release.decision_reason ?? d;
  });

  const qualityBreakdownEntries = $derived(
    release.quality_score_breakdown
      ? Object.entries(release.quality_score_breakdown).filter(
          ([key, val]) =>
            key !== 'total' &&
            key !== 'rejected' &&
            key !== 'rejected_reason' &&
            val !== 0 &&
            val !== undefined
        )
      : []
  );

  const parsedQualityEntries = $derived(
    release.parsed_quality
      ? Object.entries(release.parsed_quality).filter(
          ([, val]) => val !== null && val !== undefined && val !== '' && val !== false
        )
      : []
  );
</script>

<button
  class="release-row"
  class:is-best={release.is_best}
  type="button"
  onclick={() => (expanded = !expanded)}
>
  <div class="release-header">
    <div class="release-left">
      {#if compareMode && onToggleCompare}
        <input
          type="checkbox"
          class="compare-checkbox"
          checked={compareSelected}
          onclick={(e) => {
            e.stopPropagation();
            onToggleCompare(release);
          }}
        />
      {/if}
      <span class="release-title" title={release.release_title}>
        {release.release_title}
      </span>
    </div>
    <div class="release-badges">
      {#if release.indexer_name}
        <span class="indexer-badge">{release.indexer_name}</span>
      {/if}
      {#if release.match_score !== null}
        <span class="score-pill match" title="Match score"
          >M:{formatScore(release.match_score)}</span
        >
      {/if}
      {#if release.quality_score !== null}
        <span class="score-pill quality" title="Quality score"
          >Q:{(release.quality_score * 100).toFixed(0)}</span
        >
      {/if}
      {#if release.total_score !== null}
        <span class="score-pill total" title="Total score"
          >T:{(release.total_score * 100).toFixed(0)}</span
        >
      {/if}
      <span
        class="decision-badge"
        style="background: {decisionStyle.bg}; color: {decisionStyle.color}"
      >
        {decisionStyle.label}
      </span>
      <span class="chevron" class:expanded>&#9656;</span>
    </div>
  </div>

  <div class="why-summary">{whySummary}</div>

  {#if expanded}
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <div
      class="release-details"
      role="group"
      onclick={(e) => e.stopPropagation()}
      onkeydown={(e) => e.stopPropagation()}
    >
      <!-- Parsed Quality -->
      {#if parsedQualityEntries.length > 0}
        <div class="detail-section">
          <h4 class="section-title">Parsed Quality</h4>
          <div class="kv-grid">
            {#each parsedQualityEntries as [key, val] (key)}
              <span class="kv-key">{key.replace(/_/g, ' ')}</span>
              <span class="kv-val"
                >{typeof val === 'object' ? JSON.stringify(val) : String(val)}</span
              >
            {/each}
          </div>
        </div>
      {/if}

      <!-- Quality Score Breakdown -->
      {#if qualityBreakdownEntries.length > 0}
        <div class="detail-section">
          <h4 class="section-title">
            Quality Score Breakdown
            {#if release.quality_score_breakdown}
              <span class="breakdown-total">
                Total: {release.quality_score_breakdown.total.toFixed(2)}
                {#if release.quality_score_breakdown.rejected}
                  <span class="rejected-tag">REJECTED</span>
                {/if}
              </span>
            {/if}
          </h4>
          <div class="breakdown-list">
            {#each qualityBreakdownEntries as [key, val] (key)}
              <div class="breakdown-item" class:negative={typeof val === 'number' && val < 0}>
                <span class="breakdown-label">{key.replace(/_/g, ' ')}</span>
                <span class="breakdown-value"
                  >{typeof val === 'number' ? (val > 0 ? '+' : '') + val.toFixed(2) : val}</span
                >
              </div>
            {/each}
          </div>
          {#if release.quality_score_breakdown?.rejected_reason}
            <div class="rejection-reason">
              {release.quality_score_breakdown.rejected_reason}
            </div>
          {/if}
        </div>
      {/if}

      <!-- Structural Result -->
      {#if release.structural_result?.checked}
        <div class="detail-section">
          <h4 class="section-title">
            Structural Match
            <span
              class="struct-badge"
              class:matched={release.structural_result.matched}
              class:rejected={!release.structural_result.matched}
            >
              {release.structural_result.matched ? 'Matched' : 'Rejected'}
            </span>
          </h4>
          <div class="kv-grid">
            {#if release.structural_result.parsed_title !== undefined}
              <span class="kv-key">parsed title</span>
              <span class="kv-val">{release.structural_result.parsed_title ?? '-'}</span>
            {/if}
            {#if release.structural_result.parsed_season !== undefined || release.structural_result.want_season !== undefined}
              <span class="kv-key">season</span>
              <span class="kv-val">
                parsed: {release.structural_result.parsed_season ?? '-'} / want: {release
                  .structural_result.want_season ?? '-'}
              </span>
            {/if}
            {#if release.structural_result.parsed_episode !== undefined || release.structural_result.want_episode !== undefined}
              <span class="kv-key">episode</span>
              <span class="kv-val">
                parsed: {release.structural_result.parsed_episode ?? '-'} / want: {release
                  .structural_result.want_episode ?? '-'}
              </span>
            {/if}
            {#if release.structural_result.parsed_year !== undefined || release.structural_result.want_year !== undefined}
              <span class="kv-key">year</span>
              <span class="kv-val">
                parsed: {release.structural_result.parsed_year ?? '-'} / want: {release
                  .structural_result.want_year ?? '-'}
              </span>
            {/if}
          </div>
          {#if release.structural_result.reject_reason}
            <div class="rejection-reason">{release.structural_result.reject_reason}</div>
          {/if}
        </div>
      {/if}

      {#if release.match_explanation}
        {@const ex = release.match_explanation}
        <div class="detail-section">
          <h4 class="section-title">Match Explanation</h4>
          <div class="kv-grid">
            {#each ex.matched as hit, i (i)}
              <span class="kv-key">{hit.slot}</span>
              <span class="kv-val">
                {hit.value}{#if hit.alias && hit.alias !== hit.value}
                  <span class="alias">on "{hit.alias}"</span>{/if}{#if hit.weak}
                  <span class="alias">(weak)</span>{/if}
              </span>
            {/each}
            {#if ex.unexplained.length > 0}
              <span class="kv-key">unexplained</span>
              <span class="kv-val">{ex.unexplained.join(' ')}</span>
            {/if}
          </div>
          {#each ex.contradictions as c, i (i)}
            <div class="rejection-reason">other {c.slot}: {c.value}</div>
          {/each}
        </div>
      {/if}

      <!-- Match Details -->
      {#if release.match_mode || release.matched_pattern || release.match_score !== null}
        <div class="detail-section">
          <h4 class="section-title">Match Details</h4>
          <div class="kv-grid">
            {#if release.match_mode}
              <span class="kv-key">mode</span>
              <span class="kv-val">{release.match_mode}</span>
            {/if}
            {#if release.matched_pattern}
              <span class="kv-key">pattern matched</span>
              <span class="kv-val"><code>{release.matched_pattern}</code></span>
            {/if}
            {#if release.match_score !== null}
              <span class="kv-key">raw score</span>
              <span class="kv-val">{release.match_score.toFixed(4)}</span>
            {/if}
          </div>
        </div>
      {/if}

      <!-- Release Metadata -->
      <div class="detail-section">
        <h4 class="section-title">Release Metadata</h4>
        <div class="kv-grid">
          {#if release.release_size !== null}
            <span class="kv-key">size</span>
            <span class="kv-val">{formatBytes(release.release_size)}</span>
          {/if}
          {#if release.protocol}
            <span class="kv-key">protocol</span>
            <span class="kv-val">{release.protocol}</span>
          {/if}
          {#if release.release_guid}
            <span class="kv-key">guid</span>
            <span class="kv-val guid">{release.release_guid}</span>
          {/if}
          {#if release.episode_key}
            <span class="kv-key">episode key</span>
            <span class="kv-val">{release.episode_key}</span>
          {/if}
        </div>
      </div>
    </div>
  {/if}
</button>

<style>
  .alias {
    color: var(--muted);
  }

  .release-row {
    display: block;
    width: 100%;
    text-align: left;
    background: var(--card);
    border: 1px solid var(--ring);
    border-radius: 10px;
    padding: 10px 14px;
    cursor: pointer;
    color: inherit;
    font: inherit;
    transition: border-color 0.15s;
  }

  .release-row:hover {
    border-color: var(--accent);
  }

  .release-row.is-best {
    border-color: rgba(34, 197, 94, 0.4);
    background: rgba(34, 197, 94, 0.04);
  }

  .release-header {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 12px;
  }

  .release-left {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: flex-start;
    gap: 8px;
  }

  .compare-checkbox {
    flex-shrink: 0;
    margin-top: 2px;
    cursor: pointer;
  }

  .release-title {
    font-size: 13px;
    font-weight: 500;
    word-break: break-word;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }

  .release-badges {
    display: flex;
    align-items: center;
    gap: 4px;
    flex-shrink: 0;
    flex-wrap: wrap;
    justify-content: flex-end;
  }

  .indexer-badge {
    font-size: 10px;
    padding: 2px 6px;
    border-radius: 4px;
    background: var(--bg);
    border: 1px solid var(--ring);
    color: var(--muted);
  }

  .score-pill {
    font-size: 10px;
    padding: 2px 5px;
    border-radius: 4px;
    font-weight: 500;
    font-variant-numeric: tabular-nums;
  }

  .score-pill.match {
    background: rgba(59, 130, 246, 0.12);
    color: #3b82f6;
  }

  .score-pill.quality {
    background: rgba(168, 85, 247, 0.12);
    color: #a855f7;
  }

  .score-pill.total {
    background: rgba(234, 179, 8, 0.12);
    color: #eab308;
  }

  .decision-badge {
    font-size: 10px;
    padding: 2px 6px;
    border-radius: 4px;
    font-weight: 500;
    text-transform: capitalize;
    white-space: nowrap;
  }

  .chevron {
    font-size: 12px;
    color: var(--muted);
    transition: transform 0.15s;
    display: inline-block;
  }

  .chevron.expanded {
    transform: rotate(90deg);
  }

  .why-summary {
    margin-top: 4px;
    font-size: 11px;
    color: var(--muted);
  }

  .release-details {
    margin-top: 10px;
    padding-top: 10px;
    border-top: 1px solid var(--ring);
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .detail-section {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .section-title {
    margin: 0;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    color: var(--muted);
    font-weight: 600;
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .breakdown-total {
    font-size: 11px;
    font-weight: 500;
    text-transform: none;
    letter-spacing: 0;
    color: var(--ink);
  }

  .rejected-tag {
    font-size: 9px;
    padding: 1px 4px;
    border-radius: 3px;
    background: rgba(239, 68, 68, 0.15);
    color: #ef4444;
    font-weight: 600;
  }

  .kv-grid {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 2px 12px;
    font-size: 12px;
  }

  .kv-key {
    color: var(--muted);
    text-transform: capitalize;
    white-space: nowrap;
  }

  .kv-val {
    color: var(--ink);
    word-break: break-word;
  }

  .kv-val.guid {
    font-size: 10px;
    font-family: monospace;
    opacity: 0.7;
  }

  .kv-val code {
    font-size: 11px;
    padding: 1px 4px;
    background: var(--bg);
    border-radius: 3px;
    border: 1px solid var(--ring);
  }

  .breakdown-list {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .breakdown-item {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 2px 8px;
    border-radius: 4px;
    font-size: 12px;
  }

  .breakdown-item:nth-child(odd) {
    background: rgba(255, 255, 255, 0.02);
  }

  :global([data-theme='light']) .breakdown-item:nth-child(odd) {
    background: rgba(0, 0, 0, 0.02);
  }

  .breakdown-item.negative {
    color: #ef4444;
  }

  .breakdown-label {
    text-transform: capitalize;
    color: var(--muted);
  }

  .breakdown-item.negative .breakdown-label {
    color: #ef4444;
  }

  .breakdown-value {
    font-variant-numeric: tabular-nums;
    font-weight: 500;
  }

  .struct-badge {
    font-size: 10px;
    padding: 1px 5px;
    border-radius: 3px;
    font-weight: 500;
    text-transform: none;
    letter-spacing: 0;
  }

  .struct-badge.matched {
    background: rgba(34, 197, 94, 0.15);
    color: #22c55e;
  }

  .struct-badge.rejected {
    background: rgba(239, 68, 68, 0.15);
    color: #ef4444;
  }

  .rejection-reason {
    padding: 4px 8px;
    background: rgba(239, 68, 68, 0.08);
    border-radius: 4px;
    font-size: 11px;
    color: #ef4444;
  }

  @media (max-width: 640px) {
    .release-header {
      flex-direction: column;
      gap: 6px;
    }

    .release-badges {
      justify-content: flex-start;
    }
  }
</style>
