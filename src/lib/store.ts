import { writable, get } from "svelte/store";

export interface EmulatorView {
  id: string;
  name: string;
  hint: string;
  source_path: string;
  dest_path: string;
  enabled: boolean;
  watching: boolean;
  proc_watching: boolean;
  process_name: string;
  last_sync: string | null;
  last_error: string | null;
}

export const emulators = writable<EmulatorView[]>([]);

export function hydrateFromList(list: EmulatorView[]) {
  emulators.set(list);
}

export function applyChanged(updated: EmulatorView) {
  emulators.update((list) => {
    const idx = list.findIndex((e) => e.id === updated.id);
    if (idx === -1) return [...list, updated];
    const next = [...list];
    next[idx] = updated;
    return next;
  });
}

export function getEmulator(id: string): EmulatorView | undefined {
  return get(emulators).find((e) => e.id === id);
}
