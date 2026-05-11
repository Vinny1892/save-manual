// Tauri doesn't have a Node.js server to do proper SSR
// so we use adapter-static with a fallback to index.html to put the site in SPA mode
// See: https://svelte.dev/docs/kit/single-page-apps
// See: https://v2.tauri.app/start/frontend/sveltekit/ for more info
export const ssr = false;

import { waitLocale } from "svelte-i18n";
import { locale } from "$lib/i18n";
import { get } from "svelte/store";

/**
 * Block first render until the active locale's dict is loaded. Without
 * this, `$_("…")` calls in +layout.svelte (header, footer) throw
 * "Cannot format a message without first setting the initial locale"
 * because `register()` uses dynamic imports that resolve async.
 */
export async function load() {
  await waitLocale(get(locale) ?? undefined);
  return {};
}
