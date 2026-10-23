<script lang="ts">
  import { page } from '$app/stores';
  import NavGroup from './NavGroup.svelte';
  import { toggleGroup, isGroupExpanded } from '$lib/stores/sidebar.svelte';
  import { getReviewState } from '$lib/stores/review.svelte';

  interface Props {
    devMode?: boolean;
    koblemEnabled?: boolean;
  }

  let { devMode = false, koblemEnabled = false }: Props = $props();

  let filterQuery = $state('');

  // Alias mappings for fuzzy search
  const aliases: Record<string, string[]> = {
    search: ['find', 'query', 'indexer'],
    downloads: ['queue', 'list', 'active'],
    indexers: ['prowlarr', 'api', 'config', 'rss', 'feeds', 'sources'],
    definitions: ['cardigann', 'yaml', 'indexer definitions'],
    clients: ['transmission', 'sabnzbd', 'downloader'],
    protocol: ['usenet', 'torrent', 'priority', 'delay', 'fallback'],
    'media types': ['types', 'categories', 'routing'],
    'quality profiles': ['quality', 'resolution', 'format'],
    'upgrade defaults': ['upgrade', 'window', 'cutoff', 'quality'],
    users: ['accounts', 'login'],
    'api keys': ['tokens', 'auth', 'keys'],
    webhooks: ['hooks', 'notifications', 'callbacks', 'events'],
    'watch directories': ['watch', 'folder', 'directory', 'scan', 'monitor'],
    'system health': ['status', 'health', 'indexers', 'errors'],
    'koblem (llm)': ['llm', 'ai', 'review', 'koblem', 'local model'],
    'dev tools': ['development', 'debug', 'snapshot'],
    patterns: ['watch', 'wanted', 'monitor'],
    review: ['missed', 'doubtful', 'queue', 'alias', 'learned', 'names'],
    'match history': ['matches', 'found', 'results'],
    'search log': ['decision', 'log', 'search decisions', 'backlog'],
    'feed log': ['rss', 'feed', 'sync', 'feed decisions'],
    'debrid log': ['debrid', 'stream', 'debrid decisions']
  };

  function matchesFilter(label: string, query: string): boolean {
    if (!query) return true;
    const q = query.toLowerCase().trim();
    const l = label.toLowerCase();

    // Direct match
    if (l.includes(q)) return true;

    // Check aliases
    const labelAliases = aliases[l] || [];
    for (const alias of labelAliases) {
      if (alias.includes(q)) return true;
    }

    // Check if query is an alias pointing to this label
    for (const [key, vals] of Object.entries(aliases)) {
      if (vals.some((v) => v.includes(q)) && key === l) return true;
    }

    return false;
  }

  // Filter items based on query
  function filterItems(
    items: { href: string; label: string; icon: string; badge?: number }[]
  ): { href: string; label: string; icon: string; badge?: number }[] {
    if (!filterQuery.trim()) return items;
    return items.filter((item) => matchesFilter(item.label, filterQuery));
  }

  // Navigation structure
  const downloadItems = [
    {
      href: '/search',
      label: 'Search',
      icon: 'M15.5 14h-.79l-.28-.27A6.471 6.471 0 0 0 16 9.5 6.5 6.5 0 1 0 9.5 16c1.61 0 3.09-.59 4.23-1.57l.27.28v.79l5 4.99L20.49 19l-4.99-5zm-6 0C7.01 14 5 11.99 5 9.5S7.01 5 9.5 5 14 7.01 14 9.5 11.99 14 9.5 14z'
    },
    {
      href: '/downloads',
      label: 'Downloads',
      icon: 'M19 9h-4V3H9v6H5l7 7 7-7zM5 18v2h14v-2H5z'
    }
  ];

  const review = getReviewState();

  const watchItems = $derived([
    {
      href: '/watch/patterns',
      label: 'Patterns',
      icon: 'M12 4.5C7 4.5 2.73 7.61 1 12c1.73 4.39 6 7.5 11 7.5s9.27-3.11 11-7.5c-1.73-4.39-6-7.5-11-7.5zM12 17c-2.76 0-5-2.24-5-5s2.24-5 5-5 5 2.24 5 5-2.24 5-5 5zm0-8c-1.66 0-3 1.34-3 3s1.34 3 3 3 3-1.34 3-3-1.34-3-3-3z'
    },
    {
      href: '/watch/history',
      label: 'Match History',
      icon: 'M13 3a9 9 0 0 0-9 9H1l3.89 3.89.07.14L9 12H6c0-3.87 3.13-7 7-7s7 3.13 7 7-3.13 7-7 7c-1.93 0-3.68-.79-4.94-2.06l-1.42 1.42A8.954 8.954 0 0 0 13 21a9 9 0 0 0 0-18zm-1 5v5l4.28 2.54.72-1.21-3.5-2.08V8H12z'
    },
    {
      href: '/watch/review',
      label: 'Review',
      icon: 'M19 3H5c-1.1 0-2 .9-2 2v14c0 1.1.9 2 2 2h14c1.1 0 2-.9 2-2V5c0-1.1-.9-2-2-2zm-7 14h-2v-2h2v2zm0-4h-2V7h2v6z',
      badge: review.openCount
    }
  ]);

  const debugItems = [
    {
      href: '/watch/search-log',
      label: 'Search Log',
      icon: 'M15.5 14h-.79l-.28-.27A6.471 6.471 0 0 0 16 9.5 6.5 6.5 0 1 0 9.5 16c1.61 0 3.09-.59 4.23-1.57l.27.28v.79l5 4.99L20.49 19l-4.99-5zm-6 0C7.01 14 5 11.99 5 9.5S7.01 5 9.5 5 14 7.01 14 9.5 11.99 14 9.5 14z'
    },
    {
      href: '/watch/feed-log',
      label: 'Feed Log',
      icon: 'M6.18 15.64a2.18 2.18 0 0 1 2.18 2.18C8.36 19 7.38 20 6.18 20C5 20 4 19 4 17.82a2.18 2.18 0 0 1 2.18-2.18M4 4.44A15.56 15.56 0 0 1 19.56 20h-2.83A12.73 12.73 0 0 0 4 7.27V4.44m0 5.66a9.9 9.9 0 0 1 9.9 9.9h-2.83A7.07 7.07 0 0 0 4 12.93V10.1z'
    },
    {
      href: '/watch/debrid-log',
      label: 'Debrid Log',
      icon: 'M13 10V3L4 14h7v7l9-11h-7z'
    }
  ];

  const koblemSettingsItem = {
    href: '/settings/koblem',
    label: 'Koblem (LLM)',
    icon: 'M9 21c0 .5.4 1 1 1h4c.6 0 1-.5 1-1v-1H9v1zm3-19C8.1 2 5 5.1 5 9c0 2.4 1.2 4.5 3 5.7V17c0 .5.4 1 1 1h6c.6 0 1-.5 1-1v-2.3c1.8-1.3 3-3.4 3-5.7 0-3.9-3.1-7-7-7z'
  };

  const settingsItems = [
    {
      href: '/settings/debrid',
      label: 'Debrid',
      icon: 'M13 10V3L4 14h7v7l9-11h-7z'
    },
    {
      href: '/settings/indexers',
      label: 'Indexers',
      icon: 'M15.5 14h-.79l-.28-.27A6.471 6.471 0 0 0 16 9.5 6.5 6.5 0 1 0 9.5 16c1.61 0 3.09-.59 4.23-1.57l.27.28v.79l5 4.99L20.49 19l-4.99-5zm-6 0C7.01 14 5 11.99 5 9.5S7.01 5 9.5 5 14 7.01 14 9.5 11.99 14 9.5 14z'
    },
    {
      href: '/settings/definitions',
      label: 'Definitions',
      icon: 'M14 2H6c-1.1 0-2 .9-2 2v16c0 1.1.9 2 2 2h12c1.1 0 2-.9 2-2V8l-6-6zm-1 7V3.5L18.5 9H13z'
    },
    {
      href: '/settings/clients',
      label: 'Clients',
      icon: 'M19 9h-4V3H9v6H5l7 7 7-7zM5 18v2h14v-2H5z'
    },
    {
      href: '/settings/protocol',
      label: 'Protocol',
      icon: 'M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm-1 17.93c-3.95-.49-7-3.85-7-7.93 0-.62.08-1.21.21-1.79L9 15v1c0 1.1.9 2 2 2v1.93zm6.9-2.54c-.26-.81-1-1.39-1.9-1.39h-1v-3c0-.55-.45-1-1-1H8v-2h2c.55 0 1-.45 1-1V7h2c1.1 0 2-.9 2-2v-.41c2.93 1.19 5 4.06 5 7.41 0 2.08-.8 3.97-2.1 5.39z'
    },
    {
      href: '/settings/media-types',
      label: 'Media Types',
      icon: 'M4 4h16v2H4V4zm0 4h16v3H4V8zm0 5h16v3H4v-3zm0 5h16v2H4v-2z'
    },
    {
      href: '/settings/quality-profiles',
      label: 'Quality Profiles',
      icon: 'M19 3H5c-1.1 0-2 .9-2 2v14c0 1.1.9 2 2 2h14c1.1 0 2-.9 2-2V5c0-1.1-.9-2-2-2zm-7 14l-5-5 1.41-1.41L12 14.17l7.59-7.59L21 8l-9 9z'
    },
    {
      href: '/settings/upgrade-defaults',
      label: 'Upgrade Defaults',
      icon: 'M16 6l2.29 2.29-4.88 4.88-4-4L2 16.59 3.41 18l6-6 4 4 6.3-6.29L22 12V6z'
    },
    {
      href: '/settings/users',
      label: 'Users',
      icon: 'M16 11c1.66 0 3-1.34 3-3s-1.34-3-3-3-3 1.34-3 3 1.34 3 3 3zm-8 0c1.66 0 3-1.34 3-3S9.66 5 8 5 5 6.34 5 8s1.34 3 3 3zm0 2c-2.33 0-7 1.17-7 3.5V19h14v-2.5c0-2.33-4.67-3.5-7-3.5zm8 0c-.29 0-.62.02-.97.05 1.16.84 1.97 2 1.97 3.45V19h6v-2.5c0-2.33-4.67-3.5-7-3.5z'
    },
    {
      href: '/settings/api-keys',
      label: 'API Keys',
      icon: 'M12.65 10C11.83 7.67 9.61 6 7 6c-3.31 0-6 2.69-6 6s2.69 6 6 6c2.61 0 4.83-1.67 5.65-4H17v4h4v-4h2v-4H12.65zM7 14c-1.1 0-2-.9-2-2s.9-2 2-2 2 .9 2 2-.9 2-2 2z'
    },
    {
      href: '/settings/webhooks',
      label: 'Webhooks',
      icon: 'M7.5 4C4.46 4 2 6.46 2 9.5S4.46 15 7.5 15H11v2H7.5C3.36 17 0 13.64 0 9.5S3.36 2 7.5 2 15 5.36 15 9.5h-2C13 6.46 10.54 4 7.5 4zm9 2C19.54 6 22 8.46 22 11.5S19.54 17 16.5 17H13v-2h3.5c2.04 0 4-1.96 4-4s-1.96-4-4-4H13V5h3.5z'
    },
    {
      href: '/settings/watch-directories',
      label: 'Watch Directories',
      icon: 'M10 4H4c-1.1 0-2 .9-2 2v12c0 1.1.9 2 2 2h16c1.1 0 2-.9 2-2V8c0-1.1-.9-2-2-2h-8l-2-2z'
    },
    {
      href: '/settings/health',
      label: 'System Health',
      icon: 'M16 6l2.29 2.29-4.88 4.88-4-4L2 16.59 3.41 18l6-6 4 4 6.3-6.29L22 12V6z'
    }
  ];

  const devItems = [
    {
      href: '/settings/dev-tools',
      label: 'Dev Tools',
      icon: 'M22.7 19l-9.1-9.1c.9-2.3.4-5-1.5-6.9-2-2-5-2.4-7.4-1.3L9 6 6 9 1.6 4.7C.4 7.1.9 10.1 2.9 12.1c1.9 1.9 4.6 2.4 6.9 1.5l9.1 9.1c.4.4 1 .4 1.4 0l2.3-2.3c.5-.4.5-1.1.1-1.4z'
    }
  ];

  // Filtered items
  const filteredDownloadItems = $derived(filterItems(downloadItems));
  const filteredWatchItems = $derived(filterItems(watchItems));
  const filteredSettingsItems = $derived(
    filterItems(koblemEnabled ? [koblemSettingsItem, ...settingsItems] : settingsItems)
  );
  const filteredDebugItems = $derived(filterItems(debugItems));
  const filteredDevItems = $derived(devMode ? filterItems(devItems) : []);
  const isFiltering = $derived(filterQuery.trim().length > 0);

  function isHomeActive(): boolean {
    return $page.url.pathname === '/dashboard';
  }
</script>

<aside class="sidebar">
  <div class="sidebar-content">
    <!-- Home link -->
    <a href="/dashboard" class="home-link" class:active={isHomeActive()} title="Home">
      <svg viewBox="0 0 24 24" width="18" height="18" fill="currentColor">
        <path d="M10 20v-6h4v6h5v-8h3L12 3 2 12h3v8z"></path>
      </svg>
      <span>Home</span>
    </a>

    <!-- Filter input -->
    <div class="sidebar-filter">
      <svg viewBox="0 0 24 24" width="14" height="14" fill="currentColor" class="filter-icon">
        <path
          d="M15.5 14h-.79l-.28-.27A6.471 6.471 0 0 0 16 9.5 6.5 6.5 0 1 0 9.5 16c1.61 0 3.09-.59 4.23-1.57l.27.28v.79l5 4.99L20.49 19l-4.99-5zm-6 0C7.01 14 5 11.99 5 9.5S7.01 5 9.5 5 14 7.01 14 9.5 11.99 14 9.5 14z"
        ></path>
      </svg>
      <input
        type="text"
        class="filter-input"
        placeholder="Filter..."
        bind:value={filterQuery}
        autocomplete="off"
      />
      {#if filterQuery}
        <button
          class="filter-clear"
          type="button"
          onclick={() => (filterQuery = '')}
          aria-label="Clear filter"
        >
          <svg viewBox="0 0 24 24" width="12" height="12" fill="currentColor">
            <path
              d="M19 6.41L17.59 5 12 10.59 6.41 5 5 6.41 10.59 12 5 17.59 6.41 19 12 13.41 17.59 19 19 17.59 13.41 12z"
            ></path>
          </svg>
        </button>
      {/if}
    </div>

    <div class="sidebar-divider"></div>

    <!-- Navigation groups -->
    {#if filteredDownloadItems.length > 0}
      <NavGroup
        title="Downloads"
        items={filteredDownloadItems}
        expanded={isFiltering || isGroupExpanded('downloads')}
        collapsed={false}
        onToggle={() => toggleGroup('downloads')}
      />
    {/if}

    {#if filteredWatchItems.length > 0}
      <NavGroup
        title="Watch"
        items={filteredWatchItems}
        expanded={isFiltering || isGroupExpanded('watch')}
        collapsed={false}
        onToggle={() => toggleGroup('watch')}
      />
    {/if}

    {#if filteredDebugItems.length > 0}
      <NavGroup
        title="Debug"
        items={filteredDebugItems}
        expanded={isFiltering || isGroupExpanded('debug')}
        collapsed={false}
        onToggle={() => toggleGroup('debug')}
      />
    {/if}

    {#if filteredSettingsItems.length > 0}
      <NavGroup
        title="Settings"
        items={filteredSettingsItems}
        expanded={isFiltering || isGroupExpanded('settings')}
        collapsed={false}
        onToggle={() => toggleGroup('settings')}
      />
    {/if}

    {#if filteredDevItems.length > 0}
      <NavGroup
        title="Development"
        items={filteredDevItems}
        expanded={isFiltering || isGroupExpanded('dev')}
        collapsed={false}
        onToggle={() => toggleGroup('dev')}
      />
    {/if}

    {#if isFiltering && filteredDownloadItems.length === 0 && filteredWatchItems.length === 0 && filteredDebugItems.length === 0 && filteredSettingsItems.length === 0 && filteredDevItems.length === 0}
      <div class="no-results">No matches</div>
    {/if}
  </div>
</aside>

<style>
  .sidebar {
    display: flex;
    flex-direction: column;
    width: 220px;
    background: var(--card);
    border-right: 1px solid var(--ring);
    height: 100%;
    overflow: hidden;
    flex-shrink: 0;
  }

  .sidebar-content {
    flex: 1;
    overflow-y: auto;
    overflow-x: hidden;
    padding: 12px 0;
  }

  /* Home link */
  .home-link {
    display: flex;
    align-items: center;
    gap: 10px;
    margin: 0 8px 4px;
    padding: 10px 12px;
    border-radius: 10px;
    color: var(--ink);
    text-decoration: none;
    font-size: 14px;
    font-weight: 500;
    transition:
      background 0.15s,
      color 0.15s;
  }

  .home-link:hover {
    background: rgba(255, 255, 255, 0.06);
  }

  :global([data-theme='light']) .home-link:hover {
    background: rgba(0, 0, 0, 0.06);
  }

  .home-link.active {
    background: rgba(211, 140, 90, 0.16);
    color: var(--accent);
  }

  :global([data-theme='light']) .home-link.active {
    background: rgba(200, 122, 70, 0.18);
  }

  .home-link svg {
    flex-shrink: 0;
    opacity: 0.8;
  }

  .home-link.active svg {
    opacity: 1;
  }

  /* Filter input */
  .sidebar-filter {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 4px 8px 8px;
    padding: 6px 10px;
    background: rgba(255, 255, 255, 0.04);
    border: 1px solid var(--ring);
    border-radius: 8px;
  }

  :global([data-theme='light']) .sidebar-filter {
    background: rgba(0, 0, 0, 0.04);
  }

  .filter-icon {
    flex-shrink: 0;
    color: var(--muted);
  }

  .filter-input {
    flex: 1;
    background: transparent;
    border: none;
    color: var(--ink);
    font-size: 12px;
    outline: none;
    min-width: 0;
  }

  .filter-input::placeholder {
    color: var(--muted);
  }

  .filter-clear {
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 2px;
    background: transparent;
    border: none;
    border-radius: 4px;
    color: var(--muted);
    cursor: pointer;
  }

  .filter-clear:hover {
    background: var(--ring);
    color: var(--ink);
  }

  .no-results {
    text-align: center;
    padding: 16px;
    color: var(--muted);
    font-size: 12px;
  }

  /* Divider */
  .sidebar-divider {
    height: 1px;
    background: var(--ring);
    margin: 8px 12px;
  }

  /* Hide sidebar on mobile */
  @media (max-width: 768px) {
    .sidebar {
      display: none;
    }
  }
</style>
