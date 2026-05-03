<script lang="ts">
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import "../app.css";
  import { theme, applyStoredTheme, toggleTheme } from "$lib/theme";
  import { hydrateFromList, applyChanged, emulators } from "$lib/store";
  import { invoke } from "@tauri-apps/api/core";

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
    return d.toLocaleTimeString("pt-BR", { hour12: false });
  }
</script>

<div class="crt" data-theme={$theme}>
  <div class="scanlines"></div>
  <div class="grain"></div>

  <header class="bar" data-tauri-drag-region>
    <div class="bar-left" data-tauri-drag-region>
      <span class="led led-green"></span>
      <span class="bar-label" data-tauri-drag-region>SAVE-SYNC.SYS</span>
      <span class="bar-meta" data-tauri-drag-region>v0.1.0 / nt-x64</span>
    </div>
    <div class="bar-right">
      <span class="bar-meta" data-tauri-drag-region>{$emulators.length} units</span>
      <span class="divider" data-tauri-drag-region>·</span>
      <span class="bar-meta clock" data-tauri-drag-region>{fmtClock(now)}</span>
      <button class="theme-toggle" onclick={toggleTheme} aria-label="toggle theme">
        {$theme === "dark" ? "[ ☼ ]" : $theme === "light" ? "[ ❄ ]" : "[ ☾ ]"}
      </button>
      <div class="wm-btns">
        <button class="wm-btn" onclick={() => win.minimize()} aria-label="minimizar">─</button>
        <button class="wm-btn" onclick={() => win.toggleMaximize()} aria-label="maximizar">□</button>
        <button class="wm-btn wm-close" onclick={() => win.close()} aria-label="fechar">×</button>
      </div>
    </div>
  </header>

  <main>
    {@render children()}

    <footer class="foot">
      <span>━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━</span>
      <span class="foot-meta">eof / ready</span>
    </footer>
  </main>
</div>
