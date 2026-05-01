<script lang="ts">
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import "../app.css";
  import { theme, applyStoredTheme, toggleTheme } from "$lib/theme";
  import { hydrateFromList, applyChanged, emulators } from "$lib/store";
  import { invoke } from "@tauri-apps/api/core";

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
    return d.toLocaleTimeString("pt-BR", { hour12: false });
  }
</script>

<div class="crt" data-theme={$theme}>
  <div class="scanlines"></div>
  <div class="grain"></div>

  <main>
    <header class="bar">
      <div class="bar-left">
        <span class="led led-green"></span>
        <span class="bar-label">SAVE-SYNC.SYS</span>
        <span class="bar-meta">v0.1.0 / nt-x64</span>
      </div>
      <div class="bar-right">
        <span class="bar-meta">{$emulators.length} units</span>
        <span class="divider">·</span>
        <span class="bar-meta clock">{fmtClock(now)}</span>
        <button class="theme-toggle" onclick={toggleTheme} aria-label="toggle theme">
          {$theme === "dark" ? "[ ☼ ]" : "[ ☾ ]"}
        </button>
      </div>
    </header>

    {@render children()}

    <footer class="foot">
      <span>━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━</span>
      <span class="foot-meta">eof / ready</span>
    </footer>
  </main>
</div>
