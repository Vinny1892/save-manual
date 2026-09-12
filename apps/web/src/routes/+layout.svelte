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
  import {
    startGamepadService,
    setGamepadHandler,
    gamepadConnected,
  } from "$lib/gamepad";
  import { handleGamepadEvent } from "$lib/gamepadNav";

  const win = getCurrentWindow();

  let { children } = $props();
  let now = $state(new Date());

  interface SyncStats {
    bytes?: number;
    totalBytes?: number;
    speed?: number;
    eta?: number;
    errors?: number;
  }
  interface SyncProgress {
    id: string;
    active: boolean;
    stats?: SyncStats;
  }
  let activeSync = $state<SyncProgress | null>(null);

  onMount(() => {
    applyStoredTheme();
    const clockInterval = setInterval(() => (now = new Date()), 1000);

    invoke<any[]>("list_emulators")
      .then((list) => hydrateFromList(list))
      .catch(() => {});

    const unEmulator = listen<any>("emulator-changed", (e) => applyChanged(e.payload));
    const unProgress = listen<SyncProgress>("sync-progress", (e) => {
      activeSync = e.payload.active ? e.payload : null;
    });

    setGamepadHandler(handleGamepadEvent);
    const stopGamepad = startGamepadService();

    return () => {
      clearInterval(clockInterval);
      unEmulator.then((fn) => fn());
      unProgress.then((fn) => fn());
      stopGamepad();
      setGamepadHandler(null);
    };
  });

  function fmtBytes(b: number | undefined): string {
    if (b == null) return "—";
    if (b < 1024) return `${b} B`;
    if (b < 1024 * 1024) return `${(b / 1024).toFixed(1)} KB`;
    if (b < 1024 * 1024 * 1024) return `${(b / 1024 / 1024).toFixed(1)} MB`;
    return `${(b / 1024 / 1024 / 1024).toFixed(2)} GB`;
  }

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
      {#if $gamepadConnected}
        <span
          class="bar-meta gamepad-indicator"
          title={$_("header.gamepad_connected")}
          aria-label={$_("header.gamepad_connected")}
        >⎚</span>
      {/if}
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

  {#if activeSync}
    <div class="sync-banner" role="status" aria-live="polite">
      <span class="sync-spinner">▣</span>
      {#if activeSync.stats && activeSync.stats.totalBytes != null && activeSync.stats.totalBytes > 0}
        <span class="sync-id">{$_("sync_banner.syncing", { values: { id: activeSync.id } })}</span>
        <span class="sync-bytes">
          {fmtBytes(activeSync.stats.bytes)} / {fmtBytes(activeSync.stats.totalBytes)}
        </span>
        {#if activeSync.stats.speed != null && activeSync.stats.speed > 0}
          <span class="sync-speed">
            {$_("sync_banner.speed", { values: { bytes: fmtBytes(activeSync.stats.speed) } })}
          </span>
        {/if}
        {#if activeSync.stats.eta != null && activeSync.stats.eta > 0}
          <span class="sync-eta">
            {$_("sync_banner.eta", { values: { n: activeSync.stats.eta } })}
          </span>
        {/if}
      {:else}
        <span class="sync-id">{$_("sync_banner.starting", { values: { id: activeSync.id } })}</span>
      {/if}
    </div>
  {/if}
</div>
