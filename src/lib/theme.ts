import { writable } from "svelte/store";

type Theme = "dark" | "light";

export const theme = writable<Theme>("dark");

const KEY = "save-sync-theme";

export function applyStoredTheme() {
  try {
    const stored = localStorage.getItem(KEY);
    if (stored === "light" || stored === "dark") theme.set(stored);
  } catch {}
}

export function toggleTheme() {
  theme.update((t) => {
    const next: Theme = t === "dark" ? "light" : "dark";
    try {
      localStorage.setItem(KEY, next);
    } catch {}
    return next;
  });
}
