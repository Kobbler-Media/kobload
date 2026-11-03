import type { LearnedAlias } from '#lib/types.js';

export function learnedNames(aliases: LearnedAlias[]): string[] {
  return aliases.map((a) => a.alias.toLowerCase());
}

export function learnedIn(text: string | null | undefined, learned: string[]): string | null {
  if (!text || learned.length === 0) return null;
  const words = ` ${text.toLowerCase().replace(/[^\p{L}\p{N}]+/gu, ' ')} `;
  return (
    learned.find((alias) => {
      const name = alias.replace(/[^\p{L}\p{N}]+/gu, ' ').trim();
      return name.length > 0 && words.includes(` ${name} `);
    }) ?? null
  );
}
