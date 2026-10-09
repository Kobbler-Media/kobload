/**
 * Media kind icons (16x16 inline SVGs) and source system color generation.
 */

const icons: Record<string, string> = {
  tv: `<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><rect x="1.5" y="3" width="13" height="9" rx="1.5"/><line x1="5" y1="14" x2="11" y2="14"/><line x1="8" y1="12" x2="8" y2="14"/></svg>`,
  movie: `<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><rect x="2" y="2.5" width="12" height="11" rx="1.5"/><line x1="2" y1="5.5" x2="14" y2="5.5"/><line x1="5" y1="2.5" x2="5" y2="5.5"/><line x1="8" y1="2.5" x2="8" y2="5.5"/><line x1="11" y1="2.5" x2="11" y2="5.5"/><polygon points="6.5,8 6.5,12 11,10" fill="currentColor" stroke="none"/></svg>`,
  book: `<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M2 2.5h4.5c.83 0 1.5.67 1.5 1.5v9.5c0-.83-.67-1.5-1.5-1.5H2z"/><path d="M14 2.5H9.5C8.67 2.5 8 3.17 8 4v9.5c0-.83.67-1.5 1.5-1.5H14z"/></svg>`,
  audiobook: `<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M4 12V7a4 4 0 0 1 8 0v5"/><path d="M2 10v2a2 2 0 0 0 2 2h0"/><path d="M14 10v2a2 2 0 0 1-2 2h0"/><rect x="2" y="9" width="2.5" height="3" rx="1"/><rect x="11.5" y="9" width="2.5" height="3" rx="1"/></svg>`,
  music: `<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="4.5" cy="12" r="2"/><circle cx="12.5" cy="10.5" r="2"/><line x1="6.5" y1="12" x2="6.5" y2="3"/><line x1="14.5" y1="10.5" x2="14.5" y2="1.5"/><path d="M6.5 3l8-1.5"/></svg>`,
  game: `<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M2 6.5C2 5.12 3.12 4 4.5 4h7c1.38 0 2.5 1.12 2.5 2.5v3c0 1.38-1.12 2.5-2.5 2.5h-7A2.5 2.5 0 0 1 2 9.5z"/><line x1="5" y1="7" x2="5" y2="9"/><line x1="4" y1="8" x2="6" y2="8"/><circle cx="10.5" cy="7.5" r="0.5" fill="currentColor"/><circle cx="12" cy="8.5" r="0.5" fill="currentColor"/></svg>`,
  software: `<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><polyline points="5 5.5 2.5 8 5 10.5"/><polyline points="11 5.5 13.5 8 11 10.5"/><line x1="9" y1="3.5" x2="7" y2="12.5"/></svg>`,
  sport: `<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="8" cy="8" r="6"/><path d="M8 2a8.5 8.5 0 0 1 0 12"/><path d="M8 2a8.5 8.5 0 0 0 0 12"/><line x1="2" y1="8" x2="14" y2="8"/></svg>`,
  comic: `<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><rect x="2" y="2" width="12" height="12" rx="1.5"/><line x1="8" y1="2" x2="8" y2="14"/><line x1="2" y1="8" x2="8" y2="8"/></svg>`
};

const kindToIcon: Record<string, string> = {
  tv_episode: 'tv',
  tv_season: 'tv',
  movie: 'movie',
  book: 'book',
  audiobook: 'audiobook',
  music: 'music',
  game: 'game',
  software: 'software',
  sport_event: 'sport',
  sport_season: 'sport',
  comic: 'comic'
};

/** Returns inline SVG markup for a media kind, or empty string for unknown kinds. */
export function getMediaKindIcon(kind: string | undefined): string {
  if (!kind) return '';
  const key = kindToIcon[kind];
  return key ? icons[key] : '';
}

// Status icons (14x14 inline SVGs)
const statusIcons: Record<string, string> = {
  // magnifying glass
  searching: `<svg xmlns="http://www.w3.org/2000/svg" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="11" cy="11" r="8"/><line x1="21" y1="21" x2="16.65" y2="16.65"/></svg>`,
  // down arrow
  downloading: `<svg xmlns="http://www.w3.org/2000/svg" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" y1="15" x2="12" y2="3"/></svg>`,
  // arrow up circle
  upgrading: `<svg xmlns="http://www.w3.org/2000/svg" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><polyline points="16 12 12 8 8 12"/><line x1="12" y1="16" x2="12" y2="8"/></svg>`,
  // check circle
  satisfied: `<svg xmlns="http://www.w3.org/2000/svg" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M22 11.08V12a10 10 0 1 1-5.93-9.14"/><polyline points="22 4 12 14.01 9 11.01"/></svg>`,
  // pause circle
  paused: `<svg xmlns="http://www.w3.org/2000/svg" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="10" y1="15" x2="10" y2="9"/><line x1="14" y1="15" x2="14" y2="9"/></svg>`,
  // hand / stop
  held: `<svg xmlns="http://www.w3.org/2000/svg" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><rect x="9" y="9" width="6" height="6" rx="1"/></svg>`
};

/** Returns inline SVG markup for an effective watch status. */
export function getStatusIcon(status: string): string {
  return statusIcons[status] ?? statusIcons['searching'];
}
