// UI state store using Svelte 5 runes

export { registerOverlay, unregisterOverlay, closeTopOverlay, hasOpenOverlays } from '@koblibs/ui';
import type { PageKey } from '#lib/types.js';

let currentPage = $state<PageKey>('downloads');
let theme = $state<'dark' | 'light'>('dark');
let menuOpen = $state(false);
let searchQuery = $state('');
let multiSelect = $state(false);
let showDetails = $state(false);
let showAuthorPanel = $state(false);
let editMode = $state(false);
let toasts = $state<{ id: number; message: string }[]>([]);

let toastId = 0;

export function getUIState() {
  return {
    get currentPage() {
      return currentPage;
    },
    get theme() {
      return theme;
    },
    get menuOpen() {
      return menuOpen;
    },
    get searchQuery() {
      return searchQuery;
    },
    get multiSelect() {
      return multiSelect;
    },
    get showDetails() {
      return showDetails;
    },
    get showAuthorPanel() {
      return showAuthorPanel;
    },
    get editMode() {
      return editMode;
    },
    get toasts() {
      return toasts;
    }
  };
}

export function setCurrentPage(page: PageKey) {
  currentPage = page;
}

export function setTheme(newTheme: 'dark' | 'light') {
  theme = newTheme;
  if (typeof document !== 'undefined') {
    document.documentElement.setAttribute('data-theme', newTheme);
    localStorage.setItem('kobload-theme', newTheme);
  }
}

export function toggleTheme() {
  setTheme(theme === 'dark' ? 'light' : 'dark');
}

export function setMenuOpen(open: boolean) {
  menuOpen = open;
}

export function setSearchQuery(query: string) {
  searchQuery = query;
}

export function setMultiSelect(multi: boolean) {
  multiSelect = multi;
}

export function setShowDetails(show: boolean) {
  showDetails = show;
}

export function setShowAuthorPanel(show: boolean) {
  showAuthorPanel = show;
}

export function setEditMode(edit: boolean) {
  editMode = edit;
}

export function showToast(message: string, duration = 3000) {
  const id = ++toastId;
  toasts = [...toasts, { id, message }];
  setTimeout(() => {
    toasts = toasts.filter((t) => t.id !== id);
  }, duration);
}

export function initTheme() {
  if (typeof localStorage !== 'undefined') {
    const saved = localStorage.getItem('kobload-theme');
    if (saved === 'light' || saved === 'dark') {
      setTheme(saved);
    }
  }
}
