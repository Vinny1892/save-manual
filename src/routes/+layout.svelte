<script lang="ts">
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import "../app.css";
  import { theme, applyStoredTheme, toggleTheme } from "$lib/theme";
  import { hydrateFromList, applyChanged, emulators } from "$lib/store";
  import { invoke } from "@tauri-apps/api/core";
  import { _, isLoading } from "svelte-i18n";
  import {
    locale,
    setLocale,
    nextLocale,
    localeLabel,
  } from "$lib/i18n";

  const win = getCurrentWindow();

  let { children } = $props();
  let now = $state(new Date());

  onMount(async () => {
    applyStoredTheme();
    setInterval(() => (now = new Date()), 1000);
    try {
      const list = await invoke<any[]>("list_emulators");
      hydrateFromList(list);
    } catch {}
    await listen<any>("emulator-changed", (e) => applyChanged(e.payload));
  });

  function fmtClock(d: Date) {
    // Clock format is intentionally locale-agnostic (24h hh:mm:ss) — that's
    // the same shape across our 3 supported locales and matches the
    // monospace/terminal aesthetic.
    return d.toLocaleTimeString("en-GB", { hour12: false });
  }

  function cycleLocale() {
    setLocale(nextLocale($locale ?? "pt-BR"));
  }
</script>

<div class="crt" data-theme={$theme}>
  <div class="scanlines"></div>
  <div class="grain"></div>

  <header class="bar" data-tauri-drag-region>
    <div class="bar-left" data-tauri-drag-region>
      <span class="led led-green"></span>
      <span class="bar-label" data-tauri-drag-region>{$_("header.app_name")}</span>
      <span class="bar-meta" data-tauri-drag-region>{$_("header.version_tag")}</span>
    </div>
    <div class="bar-right">
      <span class="bar-meta" data-tauri-drag-region>
        {$_("header.units_label", { values: { n: $emulators.length } })}
      </span>
      <span class="divider" data-tauri-drag-region>·</span>
      <span class="bar-meta clock" data-tauri-drag-region>{fmtClock(now)}</span>
      <button
        class="locale-toggle"
        onclick={cycleLocale}
        aria-label={$_("header.toggle_locale")}
        title={$_("header.toggle_locale")}
      >
        [ {localeLabel($locale ?? "pt-BR")} ]
      </button>
      <button class="theme-toggle" onclick={toggleTheme} aria-label={$_("header.toggle_theme")}>
        {$theme === "dark" ? "[ ☼ ]" : $theme === "light" ? "[ ❄ ]" : "[ ☾ ]"}
      </button>
      <div class="wm-btns">
        <button class="wm-btn" onclick={() => win.minimize()} aria-label={$_("header.minimize")}>─</button>
        <button class="wm-btn" onclick={() => win.toggleMaximize()} aria-label={$_("header.maximize")}>□</button>
        <button class="wm-btn wm-close" onclick={() => win.close()} aria-label={$_("header.close")}>×</button>
      </div>
    </div>
  </header>

  <main>
    {#if $isLoading}
      <!-- Hide content until i18n dict is loaded — prevents flash of raw keys -->
      <p style="padding: 2rem; color: var(--text-muted); font-size: 0.74rem;">// init i18n…</p>
    {:else}
      {@render children()}
    {/if}

    <footer class="foot">
      <span>━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━</span>
      <span class="foot-meta">{$_("footer.eof")}</span>
    </footer>
  </main>
</div>
