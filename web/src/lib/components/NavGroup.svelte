<script lang="ts">
  import { page } from '$app/stores';

  interface NavItem {
    href: string;
    label: string;
    icon: string;
    badge?: number;
  }

  interface Props {
    title: string;
    items: NavItem[];
    expanded: boolean;
    collapsed: boolean;
    onToggle: () => void;
    onNavigate?: () => void;
  }

  let { title, items, expanded, collapsed, onToggle, onNavigate }: Props = $props();

  function isActive(href: string): boolean {
    return $page.url.pathname.startsWith(href);
  }

  const hasActiveItem = $derived(items.some((item) => isActive(item.href)));
</script>

<div class="nav-group" class:collapsed>
  {#if !collapsed}
    <button class="group-header" type="button" onclick={onToggle} class:has-active={hasActiveItem}>
      <span class="group-title">{title}</span>
      <svg
        class="chevron"
        class:expanded
        viewBox="0 0 24 24"
        width="12"
        height="12"
        fill="currentColor"
      >
        <path d="M7.41 8.59L12 13.17l4.59-4.58L18 10l-6 6-6-6 1.41-1.41z"></path>
      </svg>
    </button>
  {/if}
  {#if expanded || collapsed}
    <nav class="group-items" class:hidden={!expanded && !collapsed}>
      {#each items as item (item.href)}
        <a
          href={item.href}
          class="nav-item"
          class:active={isActive(item.href)}
          title={item.label}
          onclick={onNavigate}
        >
          <svg viewBox="0 0 24 24" width="16" height="16" fill="currentColor">
            <path d={item.icon}></path>
          </svg>
          {#if !collapsed}
            <span class="nav-label">{item.label}</span>
          {/if}
          {#if item.badge}
            <span class="nav-badge" class:dot={collapsed}>{collapsed ? '' : item.badge}</span>
          {/if}
        </a>
      {/each}
    </nav>
  {/if}
</div>

<style>
  .nav-group {
    margin-bottom: 4px;
  }

  .group-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    width: 100%;
    padding: 6px 12px;
    background: transparent;
    border: none;
    color: var(--muted);
    font-size: 10px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    cursor: pointer;
    transition: color 0.15s;
  }

  .group-header:hover {
    color: var(--ink);
  }

  .group-header.has-active {
    color: var(--accent);
  }

  .group-title {
    flex: 1;
    text-align: left;
  }

  .chevron {
    transition: transform 0.2s;
    opacity: 0.6;
  }

  .chevron.expanded {
    transform: rotate(180deg);
  }

  .collapsed .chevron {
    display: none;
  }

  .group-items {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 0 8px;
  }

  .group-items.hidden {
    display: none;
  }

  .collapsed .group-items {
    padding: 0 4px;
  }

  .nav-item {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 12px;
    border-radius: 10px;
    color: var(--ink);
    text-decoration: none;
    font-size: 13px;
    transition:
      background 0.15s,
      color 0.15s;
  }

  .nav-item:hover {
    background: rgba(255, 255, 255, 0.06);
  }

  :global([data-theme='light']) .nav-item:hover {
    background: rgba(0, 0, 0, 0.06);
  }

  .nav-item.active {
    background: rgba(211, 140, 90, 0.16);
    color: var(--accent);
  }

  :global([data-theme='light']) .nav-item.active {
    background: rgba(200, 122, 70, 0.18);
  }

  .nav-item svg {
    flex-shrink: 0;
    opacity: 0.7;
  }

  .nav-item.active svg {
    opacity: 1;
  }

  .nav-label {
    flex: 1;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .nav-badge {
    min-width: 18px;
    padding: 0 6px;
    border-radius: 9px;
    font-size: 11px;
    line-height: 18px;
    text-align: center;
    background: rgba(211, 140, 90, 0.2);
    color: var(--accent);
  }

  .nav-badge.dot {
    min-width: 8px;
    height: 8px;
    padding: 0;
    background: var(--accent);
  }

  .collapsed .nav-item {
    justify-content: center;
    padding: 10px;
  }
</style>
