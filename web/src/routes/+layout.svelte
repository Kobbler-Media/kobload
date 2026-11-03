<script lang="ts">
  import { onMount } from 'svelte';
  import { page } from '$app/state';
  import { goto, beforeNavigate } from '$app/navigation';
  import '#lib/styles/global.css';
  import favicon from '#lib/assets/favicon.svg';
  import { Toast } from '@koblibs/ui';
  import LoginForm from '#lib/components/LoginForm.svelte';
  import NavBar from '#lib/components/NavBar.svelte';
  import Sidebar from '#lib/components/Sidebar.svelte';
  import MobileMenu from '#lib/components/MobileMenu.svelte';
  import { login, getConfig, getDevSnapshotStatus } from '#lib/api/client.js';
  import { getAuthState, loadAuthToken, setAuthToken, clearAuth } from '#lib/stores/auth.svelte.js';
  import {
    getUIState,
    setTheme,
    setMenuOpen,
    initTheme,
    hasOpenOverlays,
    closeTopOverlay
  } from '#lib/stores/ui.svelte.js';
  import { showToast, getToastMessage } from '#lib/stores/toast.svelte.js';
  import { refreshReviewCount } from '#lib/stores/review.svelte.js';

  const REVIEW_COUNT_INTERVAL_MS = 5 * 60 * 1000;

  let { children } = $props();

  const auth = getAuthState();
  const ui = getUIState();

  let loading = $state(false);
  let hydrated = $state(false);
  let devMode = $state(false);
  let koblemEnabled = $state(false);

  const toastMessage = $derived(getToastMessage());

  const isAuthed = $derived(auth.isAuthed);
  const theme = $derived(ui.theme);
  const menuOpen = $derived(ui.menuOpen);

  const handleLogin = async (email: string, password: string) => {
    loading = true;
    try {
      const resp = await login(email, password);
      setAuthToken(resp.token);
      showToast('Signed in');
      goto('/dashboard', { replace: true });
    } finally {
      loading = false;
    }
  };

  const toggleTheme = () => {
    setTheme(theme === 'dark' ? 'light' : 'dark');
  };

  const validateToken = async () => {
    try {
      await getConfig();
    } catch {
      clearAuth();
    }
  };

  const checkDevMode = async () => {
    try {
      const status = await getDevSnapshotStatus();
      devMode = status.dev_mode;
      koblemEnabled = status.koblem_enabled;
    } catch {
      devMode = false;
      koblemEnabled = false;
    }
  };

  // Back gesture dismisses the topmost modal instead of leaving the page — the
  // settings editors are full-screen sheets on mobile, where Back reads as "close".
  beforeNavigate(({ type, cancel }) => {
    if (type === 'popstate' && hasOpenOverlays()) {
      cancel();
      closeTopOverlay();
    }
  });

  $effect(() => {
    if (!isAuthed) return;
    refreshReviewCount();
    const timer = setInterval(refreshReviewCount, REVIEW_COUNT_INTERVAL_MS);
    return () => clearInterval(timer);
  });

  $effect(() => {
    const pathname = page.url.pathname;
    const pageName = pathname.slice(1).split('/')[0];
    if (pageName && isAuthed) {
      localStorage.setItem('kobload-page', pageName);
    }
  });

  onMount(async () => {
    initTheme();
    loadAuthToken();

    if (auth.token) {
      await validateToken();
      if (auth.isAuthed) {
        await checkDevMode();
      }
    }

    // Restore last page for authed users
    if (auth.isAuthed && page.url.pathname === '/') {
      const lastPage = localStorage.getItem('kobload-page') || 'dashboard';
      goto(`/${lastPage}`, { replace: true });
    }

    // Dev-only: warn when main thread is blocked >200ms
    if (import.meta.env.DEV && typeof PerformanceObserver !== 'undefined') {
      const observer = new PerformanceObserver((list) => {
        for (const entry of list.getEntries()) {
          if (entry.duration > 200) {
            console.warn(`[Long task] ${Math.round(entry.duration)}ms`, entry);
          }
        }
      });
      observer.observe({ type: 'longtask', buffered: true });
    }

    // Register service worker for offline/PWA support
    if ('serviceWorker' in navigator) {
      navigator.serviceWorker.register('/sw.js').catch((err) => {
        console.warn('Service worker registration failed:', err);
      });
    }

    hydrated = true;
  });
</script>

<svelte:head>
  <title>Kobload</title>
  <link rel="icon" href={favicon} />
  <link rel="manifest" href="/manifest.json" />
  <link rel="apple-touch-icon" href="/apple-touch-icon.png" />
  <meta name="mobile-web-app-capable" content="yes" />
  <meta name="apple-mobile-web-app-status-bar-style" content="black-translucent" />
  <meta name="color-scheme" content="dark light" />
  <meta name="theme-color" content={theme === 'dark' ? '#0f1115' : '#f5efe7'} />
</svelte:head>

<div class="app">
  <NavBar {isAuthed} onToggleTheme={toggleTheme} onToggleMenu={() => setMenuOpen(!menuOpen)} />

  <div class="app-body">
    {#if isAuthed}
      <Sidebar {devMode} {koblemEnabled} />
    {/if}

    <main class="content">
      {#if !hydrated}
        <div class="loading-card">Loading…</div>
      {:else if !isAuthed}
        <LoginForm onLogin={handleLogin} {loading} />
      {:else}
        <Toast message={toastMessage} />
        {@render children()}
      {/if}
    </main>
  </div>
</div>

<MobileMenu open={menuOpen} onClose={() => setMenuOpen(false)} {devMode} {koblemEnabled} />

<style>
  :global(:root) {
    --bg: #10110c;
    --bg-2: #171812;
    --ink: #f3f1ee;
    --muted: #9da6b0;
    --accent: #e1b15a;
    --accent-2: #8b6a2f;
    --card: #1a1b14;
    --ring: #2a3038;
    --shadow: 0 18px 40px rgba(5, 7, 10, 0.5);
    --radius: 18px;
    --tag-bg: rgba(255, 255, 255, 0.08);
    --tag-text: #c6d2da;
  }

  :global([data-theme='light']) {
    --bg: #f7f0e0;
    --bg-2: #efe4cf;
    --ink: #1f1b17;
    --muted: #5f6b72;
    --accent: #d39a3c;
    --accent-2: #8a6a2e;
    --card: #fff6e4;
    --ring: #d6c5b2;
    --shadow: 0 18px 40px rgba(28, 24, 20, 0.12);
    --tag-bg: rgba(0, 0, 0, 0.06);
    --tag-text: #5c6369;
  }

  :global(*),
  :global(*::before),
  :global(*::after) {
    box-sizing: border-box;
  }

  :global(html),
  :global(body) {
    margin: 0;
    padding: 0;
    width: 100%;
    height: 100%;
    overflow: hidden;
  }

  :global(body) {
    font-family: 'Inter', 'Segoe UI', system-ui, sans-serif;
    background: var(--bg);
    color: var(--ink);
  }

  :global(*) {
    scrollbar-width: thin;
    scrollbar-color: var(--ring) transparent;
  }

  :global(*::-webkit-scrollbar) {
    width: 8px;
    height: 8px;
  }

  :global(*::-webkit-scrollbar-track) {
    background: transparent;
  }

  :global(*::-webkit-scrollbar-thumb) {
    background: var(--ring);
    border-radius: 4px;
  }

  :global(*::-webkit-scrollbar-thumb:hover) {
    background: var(--muted);
  }

  .app {
    width: 100%;
    height: 100vh;
    height: 100dvh;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }

  .app-body {
    flex: 1;
    display: flex;
    min-height: 0;
    overflow: hidden;
  }

  .content {
    flex: 1;
    overflow-y: auto;
    overflow-x: hidden;
    padding: 24px clamp(16px, 3vw, 28px) 56px;
  }

  .loading-card {
    max-width: 320px;
    margin: 60px auto 0;
    padding: 16px;
    border-radius: var(--radius);
    border: 1px solid var(--ring);
    background: var(--card);
    text-align: center;
  }
</style>
