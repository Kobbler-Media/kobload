<script lang="ts">
  import type { DownloadItem, DownloadEvent } from '#lib/api/client.js';
  import { rescanDownload } from '#lib/api/client.js';
  import EventTimeline from '../EventTimeline.svelte';
  import { downloadEventTones } from '#lib/styles/eventTones.js';

  interface Props {
    item: DownloadItem;
    isExpanded: boolean;
    events: DownloadEvent[] | undefined;
    loadingEvents: boolean;
    onToggleExpand: (id: string) => void;
    onOpenReingest: (item: DownloadItem) => void;
    onDelete: (item: DownloadItem) => void;
  }

  let { item, isExpanded, events, loadingEvents, onToggleExpand, onOpenReingest, onDelete }: Props =
    $props();

  let rescanning = $state(false);

  const formatStatus = (value: string) =>
    value.replace(/[_-]+/g, ' ').replace(/\b\w/g, (char) => char.toUpperCase());

  const formatProgress = (value: number | null) =>
    value === null ? null : `${Math.round(value * 100)}%`;

  const formatBytes = (bytes: number) => {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
    return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
  };

  const totalSize = $derived.by(() => {
    if (item.files_json && item.files_json.length > 0) {
      return item.files_json.reduce((sum, f) => sum + f.size, 0);
    }
    const clientSize =
      item.client_data && typeof item.client_data.total_size === 'number'
        ? item.client_data.total_size
        : null;
    if (clientSize) return clientSize;
    const releaseSize =
      item.release_payload && typeof item.release_payload.size === 'number'
        ? item.release_payload.size
        : null;
    if (releaseSize) return releaseSize;
    return null;
  });

  const statusClass = (value: string) =>
    `status-${value.toLowerCase().replace(/[^a-z0-9]+/g, '-')}`;

  const isActive = (status: string) => status === 'queued' || status === 'downloading';

  const canReingest = (status: string) => status === 'completed' || status === 'failed';

  // Show rescan button when completed AND callback failed AND callback not sent
  const canRescan = (i: DownloadItem) =>
    i.status === 'completed' && i.callback_error && !i.callback_sent_at;

  async function handleRescan() {
    rescanning = true;
    try {
      await rescanDownload(item.id);
    } finally {
      rescanning = false;
    }
  }
</script>

<div
  class="download-row"
  class:expanded={isExpanded}
  role="button"
  tabindex="0"
  onclick={(e: MouseEvent) => {
    const target = e.target as HTMLElement;
    if (target.closest('a, button, input, details, summary')) return;
    onToggleExpand(item.id);
  }}
  onkeydown={(e: KeyboardEvent) => {
    if (e.key === 'Enter' || e.key === ' ') {
      const target = e.target as HTMLElement;
      if (target.closest('a, button, input, details, summary')) return;
      e.preventDefault();
      onToggleExpand(item.id);
    }
  }}
>
  <div class="row-main">
    <div class="row-info">
      <div class="row-title">{item.name}</div>
      <div class="row-meta">
        {#if item.protocol}
          <span>{item.protocol.charAt(0).toUpperCase() + item.protocol.slice(1)}</span>
        {/if}
        {#if item.api_key_name}
          <span>via {item.api_key_name}</span>
        {/if}
        {#if item.watch_pattern_source_system && item.watch_pattern_source_id}
          <a
            class="pattern-link"
            href="/watch/patterns?detail={item.watch_pattern_source_system}/{item.watch_pattern_source_id}"
            onclick={(e) => e.stopPropagation()}>View Pattern</a
          >
        {/if}
        {#if totalSize}
          <span>{formatBytes(totalSize)}</span>
        {/if}
        <span>{new Date(item.created_at).toLocaleString()}</span>
      </div>
    </div>
    <div class="row-status">
      {#if item.progress !== null && item.progress < 1}
        <div class="row-progress">
          <div class="row-progress-bar" style={`width: ${Math.round(item.progress * 100)}%`}></div>
        </div>
        <span class="row-progress-text">{formatProgress(item.progress)}</span>
      {/if}
      <span class={`status-chip ${statusClass(item.status)}`}>{formatStatus(item.status)}</span>
      {#if item.import_confirmed_at}
        <span class="state-icon imported" title="Imported">
          <svg
            viewBox="0 0 24 24"
            width="16"
            height="16"
            fill="none"
            stroke="currentColor"
            stroke-width="2.5"
          >
            <path d="M20 6L9 17l-5-5"></path>
          </svg>
        </span>
      {:else if item.status === 'completed' && item.callback_sent_at && !item.import_confirmed_at}
        <span
          class="state-icon awaiting"
          title={item.callback_error
            ? `Awaiting import (${item.callback_attempts} attempts) — ${item.callback_error}`
            : `Awaiting import${item.callback_attempts > 1 ? ` (${item.callback_attempts} attempts)` : ''}`}
        >
          <svg
            viewBox="0 0 24 24"
            width="16"
            height="16"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
          >
            <circle cx="12" cy="12" r="10"></circle>
            <path d="M12 6v6l4 2"></path>
          </svg>
        </span>
      {/if}
      {#if item.status === 'completed' && item.files_scan_error}
        <span class="state-icon warning" title="Files unavailable">
          <svg
            viewBox="0 0 24 24"
            width="16"
            height="16"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
          >
            <path
              d="M10.29 3.86L1.82 18a2 2 0 001.71 3h16.94a2 2 0 001.71-3L13.71 3.86a2 2 0 00-3.42 0z"
            ></path>
            <line x1="12" y1="9" x2="12" y2="13"></line>
            <line x1="12" y1="17" x2="12.01" y2="17"></line>
          </svg>
        </span>
      {/if}
      {#if item.callback_error && !item.callback_sent_at}
        <span class="state-icon error" title="Callback failed">
          <svg
            viewBox="0 0 24 24"
            width="16"
            height="16"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
          >
            <circle cx="12" cy="12" r="10"></circle>
            <path d="M15 9l-6 6M9 9l6 6"></path>
          </svg>
        </span>
      {/if}
    </div>
  </div>
  {#if isExpanded}
    <div class="details-panel">
      <div class="details-actions">
        {#if canRescan(item)}
          <button
            class="btn compact ghost"
            type="button"
            onclick={handleRescan}
            disabled={rescanning}
          >
            {#if rescanning}
              <svg class="spinner" viewBox="0 0 24 24" width="14" height="14">
                <circle
                  cx="12"
                  cy="12"
                  r="10"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="2"
                  stroke-dasharray="31.4 31.4"
                ></circle>
              </svg>
            {:else}
              <svg
                viewBox="0 0 24 24"
                width="14"
                height="14"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
              >
                <path
                  d="M4 4v5h.582m15.356 2A8.001 8.001 0 004.582 9m0 0H9m11 11v-5h-.581m0 0a8.003 8.003 0 01-15.357-2m15.357 2H15"
                ></path>
              </svg>
            {/if}
            Rescan
          </button>
        {/if}
        {#if canReingest(item.status)}
          <button class="btn compact ghost" type="button" onclick={() => onOpenReingest(item)}>
            <svg
              viewBox="0 0 24 24"
              width="14"
              height="14"
              fill="none"
              stroke="currentColor"
              stroke-width="2"
            >
              <path d="M1 4v6h6M23 20v-6h-6"></path>
              <path d="M20.49 9A9 9 0 0 0 5.64 5.64L1 10m22 4l-4.64 4.36A9 9 0 0 1 3.51 15"></path>
            </svg>
            Reingest
          </button>
        {/if}
        <button class="btn compact ghost danger" type="button" onclick={() => onDelete(item)}>
          <svg
            viewBox="0 0 24 24"
            width="14"
            height="14"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
          >
            <path d="M18 6L6 18M6 6l12 12"></path>
          </svg>
          {isActive(item.status) ? 'Cancel' : 'Remove'}
        </button>
      </div>
      <div class="details-section">
        <h4>Download Info</h4>
        <div class="details-grid">
          {#if item.download_client_name}
            <div>
              <span class="label">Client</span>
              <span>{item.download_client_name}</span>
            </div>
          {/if}
          {#if item.external_id}
            <div>
              <span class="label">External ID</span>
              <span class="mono">{item.external_id}</span>
            </div>
          {/if}
          {#if item.file_path}
            <div>
              <span class="label">File path</span>
              <span class="mono break-word">{item.file_path}</span>
            </div>
          {/if}
          {#if item.error}
            <div>
              <span class="label">Error</span>
              <span class="error-text">{item.error}</span>
            </div>
          {/if}
          {#if item.callback_sent_at}
            <div>
              <span class="label">Callback sent</span>
              <span>{new Date(item.callback_sent_at).toLocaleString()}</span>
            </div>
          {/if}
          {#if item.import_confirmed_at}
            <div>
              <span class="label">Import confirmed</span>
              <span>{new Date(item.import_confirmed_at).toLocaleString()}</span>
            </div>
          {/if}
        </div>
      </div>

      {#if item.metadata && Object.keys(item.metadata).length > 0}
        <details class="details-section">
          <summary>Client metadata (sent back in callback)</summary>
          <div class="metadata-display">
            {#each Object.entries(item.metadata) as [key, value] (key)}
              <div class="metadata-row">
                <span class="metadata-key">{key}</span>
                {#if typeof value === 'object' && value !== null}
                  <pre class="metadata-value nested">{JSON.stringify(value, null, 2)}</pre>
                {:else}
                  <span class="metadata-value">{String(value)}</span>
                {/if}
              </div>
            {/each}
          </div>
        </details>
      {/if}

      {#if item.files_json && item.files_json.length > 0}
        <div class="details-section">
          <h4>Files ({item.files_json.length})</h4>
          <ul class="files-list">
            {#each item.files_json as file (file.relative_path)}
              <li>
                <span class="file-path">{file.relative_path}</span>
                <span class="file-size">{formatBytes(file.size)}</span>
              </li>
            {/each}
          </ul>
        </div>
      {/if}

      {#if item.client_data}
        <details class="details-section">
          <summary
            >Download client data (raw response from {item.download_client_name ||
              'client'})</summary
          >
          <pre class="json-block">{JSON.stringify(item.client_data, null, 2)}</pre>
        </details>
      {/if}

      {#if item.release_payload}
        <details class="details-section">
          <summary>Release payload (indexer data)</summary>
          <pre class="json-block">{JSON.stringify(item.release_payload, null, 2)}</pre>
        </details>
      {/if}

      <EventTimeline {events} {loadingEvents} tones={downloadEventTones} />
    </div>
  {/if}
</div>

<style>
  .download-row {
    background: var(--card);
    padding: 10px 14px;
    cursor: pointer;
    transition: background 0.1s;
  }

  .download-row:hover {
    background: var(--bg-2);
  }

  .download-row.expanded {
    background: var(--bg-2);
  }

  .download-row:first-child {
    border-radius: 12px 12px 0 0;
  }

  .download-row:last-child {
    border-radius: 0 0 12px 12px;
  }

  .download-row:only-child {
    border-radius: 12px;
  }

  .row-main {
    display: flex;
    align-items: center;
    gap: 12px;
  }

  .row-info {
    flex: 1;
    min-width: 0;
  }

  .row-title {
    font-size: 14px;
    font-weight: 500;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .row-meta {
    font-size: 12px;
    color: var(--muted);
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }

  .row-meta > :global(span + span)::before,
  .row-meta > :global(a + span)::before,
  .row-meta > :global(span + a)::before {
    content: '·';
    margin-right: 4px;
    opacity: 0.5;
  }

  .pattern-link {
    color: var(--accent);
    text-decoration: none;
    font-weight: 500;
  }

  .pattern-link:hover {
    text-decoration: underline;
  }

  .row-status {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-shrink: 0;
  }

  .row-progress {
    width: 60px;
    height: 4px;
    background: var(--ring);
    border-radius: 2px;
    overflow: hidden;
  }

  .row-progress-bar {
    height: 100%;
    background: var(--accent);
    transition: width 0.3s ease;
  }

  .row-progress-text {
    font-size: 12px;
    color: var(--muted);
    min-width: 32px;
  }

  .state-icon {
    display: flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
  }

  .state-icon.imported {
    color: #3fb950;
  }

  .state-icon.awaiting {
    color: #ca8a04;
  }

  .state-icon.warning {
    color: #f97316;
  }

  .state-icon.error {
    color: #dc3545;
  }

  .details-actions {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }

  .details-actions .btn {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }

  .details-actions .btn.danger {
    color: #dc3545;
  }

  .details-actions .btn.danger:hover {
    background: rgba(220, 53, 69, 0.12);
  }

  .spinner {
    animation: spin 1s linear infinite;
  }

  @keyframes spin {
    from {
      transform: rotate(0deg);
    }
    to {
      transform: rotate(360deg);
    }
  }

  .error-text {
    color: #dc3545;
  }

  .details-panel {
    margin-top: 8px;
    padding-top: 12px;
    border-top: 1px solid var(--ring);
    display: grid;
    gap: 16px;
  }

  .details-section {
    display: grid;
    gap: 8px;
  }

  .details-section h4 {
    margin: 0;
    font-size: 13px;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--muted);
  }

  .details-grid {
    display: grid;
    gap: 8px;
    font-size: 13px;
  }

  .details-grid .label {
    display: block;
    color: var(--muted);
    font-size: 12px;
    text-transform: uppercase;
    letter-spacing: 0.08em;
  }

  .mono {
    font-family: monospace;
    font-size: 12px;
  }

  .break-word {
    word-break: break-all;
  }

  .files-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 4px;
    font-size: 12px;
  }

  .files-list li {
    display: flex;
    justify-content: space-between;
    gap: 8px;
    padding: 4px 8px;
    background: rgba(0, 0, 0, 0.1);
    border-radius: 4px;
  }

  :global([data-theme='light']) .files-list li {
    background: rgba(0, 0, 0, 0.05);
  }

  .file-path {
    font-family: monospace;
    word-break: break-all;
  }

  .file-size {
    color: var(--muted);
    white-space: nowrap;
  }

  .json-block {
    margin: 0;
    padding: 8px;
    background: rgba(0, 0, 0, 0.15);
    border-radius: 8px;
    font-size: 12px;
    overflow-x: auto;
    max-height: 200px;
    white-space: pre-wrap;
    word-break: break-all;
  }

  :global([data-theme='light']) .json-block {
    background: rgba(0, 0, 0, 0.05);
  }

  details summary {
    cursor: pointer;
    font-size: 13px;
    color: var(--muted);
  }

  details summary:hover {
    color: var(--ink);
  }

  .metadata-display {
    display: grid;
    gap: 6px;
    margin-top: 8px;
  }

  .metadata-row {
    display: grid;
    gap: 4px;
    padding: 6px 8px;
    background: rgba(0, 0, 0, 0.08);
    border-radius: 6px;
  }

  :global([data-theme='light']) .metadata-row {
    background: rgba(0, 0, 0, 0.04);
  }

  .metadata-key {
    font-size: 12px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--muted);
  }

  .metadata-value {
    font-size: 13px;
  }

  .metadata-value.nested {
    margin: 0;
    padding: 6px 8px;
    background: rgba(0, 0, 0, 0.1);
    border-radius: 4px;
    font-size: 12px;
    font-family: monospace;
    overflow-x: auto;
    white-space: pre-wrap;
    word-break: break-all;
  }

  :global([data-theme='light']) .metadata-value.nested {
    background: rgba(0, 0, 0, 0.05);
  }

  @media (max-width: 640px) {
    .row-main {
      gap: 8px;
    }

    .row-status {
      gap: 4px;
    }

    .row-progress {
      width: 40px;
    }

    .row-progress-text {
      display: none;
    }
  }
</style>
