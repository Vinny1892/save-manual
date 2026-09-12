import { writable } from "svelte/store";

type Theme = "dark" | "light" | "blue";

export const theme = writable<Theme>("dark");

const KEY = "save-sync-theme";

export function applyStoredTheme() {
  try {
    const stored = localStorage.getItem(KEY);
    if (stored === "light" || stored === "dark" || stored === "blue") {
      theme.set(stored);
    }
  } catch {}
}

export function toggleTheme() {
  theme.update((t) => {
    const next: Theme =
      t === "dark" ? "light" : t === "light" ? "blue" : "dark";
    try {
      localStorage.setItem(KEY, next);
    } catch {}
    return next;
  });
}
