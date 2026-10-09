<script lang="ts">
  interface Props {
    isAuthed: boolean;
    onToggleTheme: () => void;
    onToggleMenu: () => void;
  }

  let { isAuthed, onToggleTheme, onToggleMenu }: Props = $props();
</script>

<header class="topbar">
  {#if isAuthed}
    <button class="mobile-menu" type="button" aria-label="Menu" onclick={onToggleMenu}>
      <svg viewBox="0 0 24 24" width="18" height="18" fill="currentColor">
        <path d="M4 7h16v2H4V7zm0 5h16v2H4v-2zm0 5h16v2H4v-2z"></path>
      </svg>
    </button>
  {/if}
  <a href="/dashboard" class="brand">Kobload</a>
  <div class="topbar-actions">
    <button class="icon-btn" type="button" aria-label="Toggle theme" onclick={onToggleTheme}>
      <svg viewBox="0 0 24 24" width="16" height="16" fill="currentColor">
        <path
          d="M12 2a1 1 0 0 1 1 1v2a1 1 0 1 1-2 0V3a1 1 0 0 1 1-1zm0 16a4 4 0 1 0 0-8 4 4 0 0 0 0 8zm9-5a1 1 0 0 1-1 1h-2a1 1 0 1 1 0-2h2a1 1 0 0 1 1 1zM6 12a1 1 0 0 1-1 1H3a1 1 0 1 1 0-2h2a1 1 0 0 1 1 1zm12.95-7.95a1 1 0 0 1 0 1.41l-1.4 1.4a1 1 0 0 1-1.41-1.41l1.4-1.4a1 1 0 0 1 1.41 0zM7.86 17.54a1 1 0 0 1 0 1.41l-1.4 1.4a1 1 0 1 1-1.41-1.41l1.4-1.4a1 1 0 0 1 1.41 0zM19 20a1 1 0 0 1-1 1h-2a1 1 0 1 1 0-2h2a1 1 0 0 1 1 1zM6 20a1 1 0 0 1-1 1H3a1 1 0 1 1 0-2h2a1 1 0 0 1 1 1z"
        ></path>
      </svg>
    </button>
  </div>
</header>

<style>
  .topbar {
    position: sticky;
    top: 0;
    z-index: 5;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    padding: 12px 20px;
    border-bottom: 1px solid var(--ring);
    background: linear-gradient(180deg, rgba(44, 35, 16, 0.95), rgba(25, 20, 12, 0.9));
    backdrop-filter: blur(6px);
  }

  :global([data-theme='light']) .topbar {
    background: linear-gradient(180deg, rgba(252, 241, 218, 0.95), rgba(246, 232, 198, 0.85));
  }

  .brand {
    font-family: inherit;
    font-size: 24px;
    font-weight: 700;
    letter-spacing: 0.6px;
    white-space: nowrap;
    text-decoration: none;
    color: var(--ink);
  }

  .brand:hover {
    color: var(--accent);
  }

  .topbar-actions {
    display: flex;
    gap: 10px;
    align-items: center;
  }

  .icon-btn {
    background: var(--card);
    border: 1px solid var(--ring);
    border-radius: 12px;
    padding: 8px 10px;
    cursor: pointer;
    color: var(--ink);
    display: inline-flex;
    align-items: center;
    justify-content: center;
  }

  .icon-btn:hover {
    background: var(--ring);
  }

  .mobile-menu {
    display: none;
    border: 1px solid var(--ring);
    background: var(--card);
    color: var(--ink);
    border-radius: 12px;
    padding: 8px 10px;
    cursor: pointer;
  }

  @media (max-width: 768px) {
    .topbar {
      padding: 10px 12px;
      justify-content: flex-start;
    }

    .brand {
      font-size: 20px;
    }

    .topbar-actions {
      margin-left: auto;
    }

    .mobile-menu {
      display: inline-flex;
    }
  }
</style>
