<script lang="ts">
  import { goto } from "$app/navigation";
  import { page } from "$app/stores";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { open } from "@tauri-apps/plugin-dialog";
  import { emulators, type EmulatorView } from "$lib/store";
  import { derived } from "svelte/store";
  import { onMount } from "svelte";

  interface DetectCandidate { path: string; label: string; }

  interface TitleDbStatus {
    count: number;
    last_update: string | null;
    refreshing: boolean;
    cache_path: string;
  }

  let debugMsg = $state("");
  let savingPaths = $state(false);
  let detecting = $state(false);
  let detectCandidates = $state<DetectCandidate[]>([]);
  let detectDone = $state(false);
  let edenUuid = $state<string | null>(null);
  let procNameDraft = $state("");
  let savingProcName = $state(false);
  let titleDb = $state<TitleDbStatus | null>(null);
  let titleDbErr = $state("");
  let ps2Db = $state<TitleDbStatus | null>(null);
  let ps2DbErr = $state("");

  interface HistorySettings {
    emulator_id: string;
    enabled: boolean;
    incremental_enabled: boolean;
    full_enabled: boolean;
    retention_days: number;
    retention_max_mb: number;
    bisync_initialized: boolean;
  }
  let history = $state<HistorySettings | null>(null);
  let historyErr = $state("");
  let savingHistory = $state(false);
  let historyDraft = $state<HistorySettings | null>(null);
  let allowsIncremental = $state(true);

  const current = derived(
    [emulators, page],
    ([$emulators, $page]) => $emulators.find((e) => e.id === $page.params.id),
  );

  let sourceDraft = $state("");
  let destDraft = $state("");
  let destKindDraft = $state<"local" | "rclone">("local");
  let destRemoteDraft = $state("");
  let availableRemotes = $state<string[]>([]);
  let remotesErr = $state("");
  let lastSeenId = "";

  $effect(() => {
    const emu = $current;
    if (!emu) return;
    if (emu.id !== lastSeenId) {
      sourceDraft = emu.source_path;
      destDraft = emu.dest_path;
      destKindDraft = emu.dest_kind || "local";
      destRemoteDraft = emu.dest_remote || "";
      procNameDraft = emu.process_name;
      lastSeenId = emu.id;
      detectCandidates = [];
      detectDone = false;
      edenUuid = null;
      history = null;
      historyDraft = null;
      historyErr = "";
      if (emu.id === "eden" && emu.source_path) {
        refreshEdenUuid(emu.source_path);
      }
      loadHistorySettings(emu.id);
    }
  });

  async function loadHistorySettings(id: string) {
    historyErr = "";
    try {
      const [s, allows] = await Promise.all([
        invoke<HistorySettings>("get_history_settings", { id }),
        invoke<boolean>("supports_incremental_history", { id }),
      ]);
      history = s;
      historyDraft = { ...s };
      allowsIncremental = allows;
    } catch (e) {
      historyErr = String(e);
    }
  }

  function historyDirty() {
    if (!history || !historyDraft) return false;
    return (
      history.enabled !== historyDraft.enabled ||
      history.incremental_enabled !== historyDraft.incremental_enabled ||
      history.full_enabled !== historyDraft.full_enabled ||
      history.retention_days !== historyDraft.retention_days ||
      history.retention_max_mb !== historyDraft.retention_max_mb
    );
  }

  function historyInvalid() {
    if (!historyDraft) return true;
    // When enabled, at least one mode must be picked. UI prevents this but
    // the backend will reject anyway — surface it before they hit commit.
    return (
      historyDraft.enabled &&
      !historyDraft.incremental_enabled &&
      !historyDraft.full_enabled
    );
  }

  /// Returns true iff toggling `mode` off would leave zero modes selected
  /// while history is enabled. Disables that checkbox so user can't unselect
  /// the last one.
  function lockOff(mode: "incremental" | "full"): boolean {
    if (!historyDraft || !historyDraft.enabled) return false;
    const other =
      mode === "incremental"
        ? historyDraft.full_enabled
        : historyDraft.incremental_enabled;
    const self =
      mode === "incremental"
        ? historyDraft.incremental_enabled
        : historyDraft.full_enabled;
    return self && !other; // self is on AND would be the only one left
  }

  async function saveHistory() {
    if (!historyDraft) return;
    savingHistory = true;
    historyErr = "";
    try {
      await invoke("set_history_settings", { settings: historyDraft });
      history = { ...historyDraft };
    } catch (e) {
      historyErr = String(e);
    } finally {
      savingHistory = false;
    }
  }

  async function loadRemotes() {
    remotesErr = "";
    try {
      availableRemotes = (await invoke<string[]>("rclone_list_remotes")).map((r) =>
        r.replace(/:$/, ""),
      );
    } catch (e) {
      remotesErr = String(e);
    }
  }

  async function pickFolder(target: "source" | "dest") {
    debugMsg = "";
    try {
      const selected = await open({ directory: true, multiple: false });
      if (typeof selected === "string") {
        if (target === "source") sourceDraft = selected;
        else destDraft = selected;
      }
    } catch (err) {
      debugMsg = "pickFolder: " + String(err);
    }
  }

  async function savePaths(emu: EmulatorView) {
    debugMsg = "";
    savingPaths = true;
    try {
      await invoke("set_emulator_paths", {
        id: emu.id,
        sourcePath: sourceDraft,
        destKind: destKindDraft,
        destRemote: destKindDraft === "rclone" ? destRemoteDraft : "",
        destPath: destDraft,
      });
      if (emu.id === "eden" && sourceDraft) refreshEdenUuid(sourceDraft);
    } catch (err) {
      debugMsg = "set_emulator_paths: " + String(err);
    } finally {
      savingPaths = false;
    }
  }

  async function syncNow(emu: EmulatorView) {
    debugMsg = "";
    try {
      await invoke("sync_now", { id: emu.id });
    } catch (err) {
      debugMsg = "sync_now: " + String(err);
    }
  }

  async function toggleWatch(emu: EmulatorView) {
    debugMsg = "";
    try {
      if (emu.watching) {
        await invoke("stop_watch", { id: emu.id });
      } else {
        await invoke("start_watch", { id: emu.id });
      }
    } catch (err) {
      debugMsg = "watch: " + String(err);
    }
  }

  async function toggleProcWatch(emu: EmulatorView) {
    debugMsg = "";
    try {
      if (emu.proc_watching) {
        await invoke("stop_proc_watch", { id: emu.id });
      } else {
        await invoke("start_proc_watch", { id: emu.id });
      }
    } catch (err) {
      debugMsg = "proc_watch: " + String(err);
    }
  }

  async function saveProcName(emu: EmulatorView) {
    debugMsg = "";
    savingProcName = true;
    try {
      await invoke("set_process_name", { id: emu.id, processName: procNameDraft });
    } catch (err) {
      debugMsg = "set_process_name: " + String(err);
    } finally {
      savingProcName = false;
    }
  }

  function procNameDirty(emu: EmulatorView) {
    return procNameDraft !== emu.process_name;
  }

  async function toggleEnabled(emu: EmulatorView) {
    debugMsg = "";
    try {
      await invoke("set_enabled", { id: emu.id, enabled: !emu.enabled });
    } catch (err) {
      debugMsg = "set_enabled: " + String(err);
    }
  }

  function back() {
    goto("/");
  }

  function viewSaves(emu: EmulatorView) {
    goto(`/emulator/${emu.id}/saves`);
  }

  function pathDirty(emu: EmulatorView) {
    return (
      sourceDraft !== emu.source_path ||
      destDraft !== emu.dest_path ||
      destKindDraft !== (emu.dest_kind || "local") ||
      destRemoteDraft !== (emu.dest_remote || "")
    );
  }

  async function detectPaths(emu: EmulatorView) {
    debugMsg = "";
    detecting = true;
    detectDone = false;
    detectCandidates = [];
    try {
      detectCandidates = await invoke<DetectCandidate[]>("detect_save_paths", { id: emu.id });
      detectDone = true;
    } catch (err) {
      debugMsg = "detect: " + String(err);
    } finally {
      detecting = false;
    }
  }

  function useDetected(path: string) {
    sourceDraft = path;
    detectCandidates = [];
    detectDone = false;
    const emu = $current;
    if (emu?.id === "eden") refreshEdenUuid(path);
  }

  async function refreshEdenUuid(nandPath: string) {
    edenUuid = null;
    try {
      edenUuid = await invoke<string | null>("get_eden_uuid", { nandPath });
    } catch {
      edenUuid = null;
    }
  }

  async function loadTitleDbStatus() {
    try {
      titleDb = await invoke<TitleDbStatus>("title_db_status");
    } catch (e) {
      titleDbErr = String(e);
    }
  }

  async function refreshTitleDb() {
    titleDbErr = "";
    try {
      await invoke("refresh_title_db");
      await loadTitleDbStatus();
    } catch (e) {
      titleDbErr = String(e);
      await loadTitleDbStatus();
    }
  }

  async function loadPs2DbStatus() {
    try {
      ps2Db = await invoke<TitleDbStatus>("ps2_db_status");
    } catch (e) {
      ps2DbErr = String(e);
    }
  }

  async function refreshPs2Db() {
    ps2DbErr = "";
    try {
      await invoke("refresh_ps2_db");
      await loadPs2DbStatus();
    } catch (e) {
      ps2DbErr = String(e);
      await loadPs2DbStatus();
    }
  }

  onMount(() => {
    loadTitleDbStatus();
    loadPs2DbStatus();
    loadRemotes();
    const u1 = listen("title-db-status", () => loadTitleDbStatus());
    const u2 = listen("ps2-db-status", () => loadPs2DbStatus());
    return () => {
      u1.then((fn) => fn());
      u2.then((fn) => fn());
    };
  });
</script>

<section class="topnav">
  <button class="back" onclick={back} aria-label="voltar">
    <span class="back-arrow">◀</span> back to index
  </button>
</section>

{#if !$current}
  <section class="empty">
    <p>// unit not found · <button class="link" onclick={back}>return</button></p>
  </section>
{:else}
  {@const emu = $current}

  <section class="head">
    <div class="head-row">
      <span class="led" class:led-green={emu.watching} class:led-amber={emu.enabled && !emu.watching} class:led-off={!emu.enabled}></span>
      <h1>{emu.name}</h1>
      <span class="state-tag">
        {#if !emu.enabled}
          // disabled
        {:else if emu.watching}
          // watching
        {:else}
          // idle
        {/if}
      </span>
    </div>
    <p class="head-id">unit_id :: {emu.id}</p>
  </section>

  <section class="card ops">
    <header class="card-head">
      <span class="card-tag">[ ops ]</span>
      <span class="card-meta">control surface</span>
    </header>

    <div class="ops-row">
      <button
        class="btn btn-power"
        class:on={emu.enabled}
        onclick={() => toggleEnabled(emu)}
      >
        {emu.enabled ? "[ disable unit ]" : "[ enable unit ]"}
      </button>

      <button class="btn" onclick={() => viewSaves(emu)}>
        ▤ view saves
      </button>

      <button
        class="btn"
        onclick={() => syncNow(emu)}
        disabled={!emu.enabled}
      >
        [ sync now ]
      </button>

      <button
        class="btn btn-watch"
        class:active={emu.watching}
        onclick={() => toggleWatch(emu)}
        disabled={!emu.enabled}
      >
        {emu.watching ? "[ halt watcher ]" : "[ engage watcher ]"}
      </button>
    </div>
  </section>

  {#if debugMsg}
    <section class="alert">
      <span class="alert-tag">! TRACE</span>
      <span>{debugMsg}</span>
    </section>
  {/if}

  <section class="card">
    <header class="card-head">
      <span class="card-tag">[ hint ]</span>
      <span class="card-meta">where to look</span>
    </header>
    <p class="hint">{emu.hint}</p>
  </section>

  <section class="card">
    <header class="card-head">
      <span class="card-tag">[ paths ]</span>
      <span class="card-meta">source &rarr; destination</span>
    </header>

    <div class="field">
      <label class="field-label" for="source-path">source / live save dir</label>
      <div class="field-row">
        <input
          id="source-path"
          type="text"
          class="field-input"
          bind:value={sourceDraft}
          placeholder="C:\path\to\emulator\saves"
          disabled={!emu.enabled}
        />
        <button class="btn btn-thin" onclick={() => pickFolder("source")} disabled={!emu.enabled}>
          [ browse ]
        </button>
        <button
          class="btn btn-thin btn-detect"
          onclick={() => detectPaths(emu)}
          disabled={!emu.enabled || detecting}
        >
          {detecting ? "…" : "[ detect ]"}
        </button>
      </div>

      {#if detectDone || detecting}
        <div class="detect-panel">
          {#if detecting}
            <span class="detect-status">// scanning disks…</span>
          {:else if detectCandidates.length === 0}
            <span class="detect-status">// nothing found</span>
          {:else}
            <span class="detect-status">// {detectCandidates.length} candidate{detectCandidates.length > 1 ? "s" : ""} — click to apply:</span>
            {#each detectCandidates as c (c.path)}
              <button class="detect-item" onclick={() => useDetected(c.path)}>
                <span class="detect-label">{c.label}</span>
                <span class="detect-path">{c.path}</span>
              </button>
            {/each}
          {/if}
        </div>
      {/if}
    </div>

    <div class="field">
      <span class="field-label">destination kind</span>
      <div class="kind-toggle">
        <button
          type="button"
          class="kind-opt"
          class:active={destKindDraft === "local"}
          onclick={() => (destKindDraft = "local")}
          disabled={!emu.enabled}
        >
          [ local ]
        </button>
        <button
          type="button"
          class="kind-opt"
          class:active={destKindDraft === "rclone"}
          onclick={() => (destKindDraft = "rclone")}
          disabled={!emu.enabled}
        >
          [ rclone ]
        </button>
      </div>
    </div>

    {#if destKindDraft === "rclone"}
      <div class="field">
        <label class="field-label" for="dest-remote">rclone remote</label>
        <div class="field-row">
          <select
            id="dest-remote"
            class="field-input"
            bind:value={destRemoteDraft}
            disabled={!emu.enabled}
          >
            <option value="" disabled>// selecione…</option>
            {#each availableRemotes as r (r)}
              <option value={r}>{r}</option>
            {/each}
          </select>
          <button
            class="btn btn-thin"
            onclick={() => goto("/remotes")}
            disabled={!emu.enabled}
          >
            [ manage ]
          </button>
        </div>
        {#if remotesErr}
          <p class="hint-line err">! {remotesErr}</p>
        {:else if availableRemotes.length === 0}
          <p class="hint-line">// nenhum remote — crie um em [ manage ]</p>
        {/if}
      </div>

      <div class="field">
        <label class="field-label" for="dest-path">path no remote (bucket/prefix)</label>
        <div class="field-row">
          <input
            id="dest-path"
            type="text"
            class="field-input"
            bind:value={destDraft}
            placeholder="meu-bucket/save-sync"
            disabled={!emu.enabled}
          />
        </div>
        <p class="hint-line">// será gravado em {destRemoteDraft || "<remote>"}:{destDraft || "<path>"}/{emu.id}/</p>
      </div>
    {:else}
      <div class="field">
        <label class="field-label" for="dest-path">destination / mirror dir</label>
        <div class="field-row">
          <input
            id="dest-path"
            type="text"
            class="field-input"
            bind:value={destDraft}
            placeholder="C:\path\to\backup"
            disabled={!emu.enabled}
          />
          <button class="btn btn-thin" onclick={() => pickFolder("dest")} disabled={!emu.enabled}>
            [ browse ]
          </button>
        </div>
      </div>
    {/if}

    <div class="field-actions">
      <button
        class="btn"
        onclick={() => savePaths(emu)}
        disabled={!emu.enabled || savingPaths || !pathDirty(emu)}
      >
        {savingPaths ? "saving..." : pathDirty(emu) ? "[ commit paths ]" : "[ saved ]"}
      </button>
    </div>
  </section>

  <section class="card">
    <header class="card-head">
      <span class="card-tag">[ status ]</span>
      <span class="card-meta">last operation</span>
    </header>

    {#if emu.id === "eden"}
      <div class="meta-row">
        <span class="meta-key">profile uuid</span>
        <span class="meta-val uuid-val" class:dim={!edenUuid}>
          {edenUuid ?? (emu.source_path ? "// not detected" : "// no source path")}
        </span>
      </div>
    {/if}

    <div class="meta-row">
      <span class="meta-key">last_sync</span>
      <span class="meta-val" class:dim={!emu.last_sync}>
        {emu.last_sync ?? "never"}
      </span>
    </div>

    {#if emu.last_error}
      <div class="meta-row error">
        <span class="meta-key">last_error</span>
        <span class="meta-val err">{emu.last_error}</span>
      </div>
    {/if}
  </section>

  <section class="card">
    <header class="card-head">
      <span class="card-tag">[ proc-watch ]</span>
      <span class="card-meta">sync on emulator close</span>
    </header>

    <div class="field">
      <label class="field-label" for="proc-name">process name</label>
      <div class="field-row">
        <input
          id="proc-name"
          type="text"
          class="field-input"
          bind:value={procNameDraft}
          placeholder="ex: pcsx2-qt  /  eden.exe  /  org.eden.android"
          disabled={!emu.enabled}
        />
        <button
          class="btn btn-thin"
          onclick={() => saveProcName(emu)}
          disabled={!emu.enabled || savingProcName || !procNameDirty(emu)}
        >
          {savingProcName ? "saving..." : procNameDirty(emu) ? "[ commit ]" : "[ saved ]"}
        </button>
      </div>
    </div>

    <div class="proc-watch-row">
      <button
        class="btn btn-watch"
        class:active={emu.proc_watching}
        onclick={() => toggleProcWatch(emu)}
        disabled={!emu.enabled}
      >
        {emu.proc_watching ? "[ halt proc-watch ]" : "[ engage proc-watch ]"}
      </button>
      <span class="proc-status" class:active={emu.proc_watching}>
        {emu.proc_watching ? "// monitoring — syncs when process exits" : "// idle"}
      </span>
    </div>
  </section>

  <section class="card">
    <header class="card-head">
      <span class="card-tag">[ history ]</span>
      <span class="card-meta">snapshot policy</span>
    </header>

    {#if !historyDraft}
      <p class="hint-line">// loading…</p>
    {:else}
      <div class="hist-row">
        <span class="field-label">backup</span>
        <div class="kind-toggle">
          <button
            type="button"
            class="kind-opt"
            class:active={historyDraft.enabled}
            onclick={() => (historyDraft!.enabled = true)}
          >
            [ on ]
          </button>
          <button
            type="button"
            class="kind-opt"
            class:active={!historyDraft.enabled}
            onclick={() => (historyDraft!.enabled = false)}
          >
            [ off ]
          </button>
        </div>
      </div>

      <div class="hist-row">
        <span class="field-label">modes</span>
        <div class="check-stack">
          <label class="check-row" class:disabled={!allowsIncremental || !historyDraft.enabled}>
            <input
              type="checkbox"
              bind:checked={historyDraft.incremental_enabled}
              disabled={!allowsIncremental || !historyDraft.enabled || lockOff("incremental")}
            />
            <span class="check-label">[ incremental ]</span>
            <span class="check-hint">só arquivos sobrescritos vão pra delta/</span>
          </label>
          <label class="check-row" class:disabled={!historyDraft.enabled}>
            <input
              type="checkbox"
              bind:checked={historyDraft.full_enabled}
              disabled={!historyDraft.enabled || lockOff("full")}
            />
            <span class="check-label">[ full ]</span>
            <span class="check-hint">estado completo copiado pra full/ antes de cada sync</span>
          </label>
        </div>
      </div>

      {#if !allowsIncremental}
        <p class="hint-line">// {emu.id} usa arquivo binário como unidade — incremental degeneraria em full, então só full disponível</p>
      {:else if historyDraft.incremental_enabled && historyDraft.full_enabled}
        <p class="hint-line">// ambos ligados: cada sync produz .history/&lt;ts&gt;/full/ + .history/&lt;ts&gt;/delta/ (storage 2x)</p>
      {:else if historyDraft.incremental_enabled}
        <p class="hint-line">// incremental: barato em storage, fácil reverter um save específico</p>
      {:else if historyDraft.full_enabled}
        <p class="hint-line">// full: pesado em storage, mas cada snapshot é estado completo</p>
      {/if}
      {#if historyInvalid()}
        <p class="hint-line err">! selecione pelo menos um modo enquanto history estiver ativo</p>
      {/if}

      <div class="field-grid">
        <div class="field">
          <label class="field-label" for="ret-days">retention (days)</label>
          <input
            id="ret-days"
            type="number"
            class="field-input"
            min="0"
            max="3650"
            bind:value={historyDraft.retention_days}
            disabled={!historyDraft.enabled}
          />
        </div>
        <div class="field">
          <label class="field-label" for="ret-mb">retention (max MB)</label>
          <input
            id="ret-mb"
            type="number"
            class="field-input"
            min="0"
            bind:value={historyDraft.retention_max_mb}
            disabled={!historyDraft.enabled}
          />
        </div>
      </div>

      <div class="meta-row">
        <span class="meta-key">bisync state</span>
        <span class="meta-val" class:dim={!historyDraft.bisync_initialized}>
          {historyDraft.bisync_initialized ? "initialized" : "// resync on next run"}
        </span>
      </div>

      {#if historyErr}
        <div class="meta-row error">
          <span class="meta-key">last_error</span>
          <span class="meta-val err">{historyErr}</span>
        </div>
      {/if}

      <div class="field-actions">
        <button
          class="btn"
          onclick={saveHistory}
          disabled={savingHistory || !historyDirty() || historyInvalid()}
        >
          {savingHistory ? "saving..." : historyDirty() ? "[ commit history ]" : "[ saved ]"}
        </button>
      </div>
    {/if}
  </section>

  {#if emu.id === "eden"}
    <section class="card">
      <header class="card-head">
        <span class="card-tag">[ titledb ]</span>
        <span class="card-meta">switch title-id ↔ name</span>
      </header>

      <div class="meta-row">
        <span class="meta-key">entries</span>
        <span class="meta-val" class:dim={!titleDb || titleDb.count === 0}>
          {titleDb ? titleDb.count.toLocaleString("pt-BR") : "…"}
        </span>
      </div>
      <div class="meta-row">
        <span class="meta-key">last update</span>
        <span class="meta-val" class:dim={!titleDb?.last_update}>
          {titleDb?.last_update ?? "never"}
        </span>
      </div>
      {#if titleDbErr}
        <div class="meta-row error">
          <span class="meta-key">last_error</span>
          <span class="meta-val err">{titleDbErr}</span>
        </div>
      {/if}

      <div class="field-actions">
        <button
          class="btn"
          onclick={refreshTitleDb}
          disabled={titleDb?.refreshing}
        >
          {titleDb?.refreshing ? "// downloading…" : "[ atualizar via blawar ]"}
        </button>
      </div>
    </section>
  {/if}

  {#if emu.id === "pcsx2"}
    <section class="card">
      <header class="card-head">
        <span class="card-tag">[ ps2db ]</span>
        <span class="card-meta">ps2 serial ↔ name</span>
      </header>

      <div class="meta-row">
        <span class="meta-key">entries</span>
        <span class="meta-val" class:dim={!ps2Db || ps2Db.count === 0}>
          {ps2Db ? ps2Db.count.toLocaleString("pt-BR") : "…"}
        </span>
      </div>
      <div class="meta-row">
        <span class="meta-key">last update</span>
        <span class="meta-val" class:dim={!ps2Db?.last_update}>
          {ps2Db?.last_update ?? "never"}
        </span>
      </div>
      {#if ps2DbErr}
        <div class="meta-row error">
          <span class="meta-key">last_error</span>
          <span class="meta-val err">{ps2DbErr}</span>
        </div>
      {/if}

      <div class="field-actions">
        <button
          class="btn"
          onclick={refreshPs2Db}
          disabled={ps2Db?.refreshing}
        >
          {ps2Db?.refreshing ? "// downloading…" : "[ atualizar via pcsx2 gameindex ]"}
        </button>
      </div>
    </section>
  {/if}
{/if}

<style>
  .topnav {
    margin: 1rem 0 0.6rem;
  }

  .back {
    background: transparent;
    border: 1px dashed var(--border-strong);
    color: var(--text-soft);
    font-family: inherit;
    font-size: 0.74rem;
    padding: 0.35rem 0.7rem;
    cursor: pointer;
    letter-spacing: 0.06em;
    text-transform: lowercase;
    transition: all 0.14s;
  }

  .back:hover {
    color: var(--text-bright);
    border-color: var(--text-soft);
    background: var(--hover-tint);
  }

  .back-arrow {
    color: var(--accent);
    margin-right: 0.25rem;
  }

  .empty {
    margin-top: 2rem;
    text-align: center;
    color: var(--text-muted);
    font-size: 0.85rem;
  }

  .link {
    background: none;
    border: none;
    color: var(--accent);
    cursor: pointer;
    font-family: inherit;
    font-size: inherit;
    text-decoration: underline;
    padding: 0;
  }

  .head {
    margin: 1rem 0 1.2rem;
  }

  .head-row {
    display: flex;
    align-items: center;
    gap: 0.7rem;
  }

  h1 {
    font-family: "Major Mono Display", monospace;
    font-size: 1.7rem;
    margin: 0;
    color: var(--text-bright);
    letter-spacing: 0.06em;
    text-transform: lowercase;
    text-shadow: var(--title-glow);
  }

  .state-tag {
    color: var(--text-muted);
    font-size: 0.72rem;
    font-style: italic;
    letter-spacing: 0.05em;
    margin-left: auto;
  }

  .head-id {
    margin: 0.4rem 0 0;
    font-size: 0.7rem;
    color: var(--text-faint);
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }

  .card {
    margin-top: 1.1rem;
    border: 1px solid var(--border);
    background: var(--bg-unit-1);
    padding: 0.85rem 1rem;
  }

  .card-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    border-bottom: 1px dashed var(--border);
    padding-bottom: 0.4rem;
    margin-bottom: 0.7rem;
  }

  .card-tag {
    color: var(--accent);
    font-size: 0.74rem;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }

  .card-meta {
    color: var(--text-muted);
    font-size: 0.68rem;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }

  .hint {
    margin: 0.2rem 0 0;
    font-size: 0.78rem;
    color: var(--text-soft);
    line-height: 1.5;
    white-space: pre-wrap;
    background: var(--bg-hint);
    padding: 0.55rem 0.7rem;
    border-left: 2px solid var(--border-strong);
  }

  .field {
    margin-bottom: 0.8rem;
  }

  .field:last-of-type {
    margin-bottom: 0;
  }

  .field-label {
    display: block;
    font-size: 0.68rem;
    color: var(--text-muted);
    letter-spacing: 0.08em;
    text-transform: uppercase;
    margin-bottom: 0.3rem;
  }

  .field-row {
    display: flex;
    gap: 0.5rem;
    align-items: stretch;
  }

  .field-input {
    flex: 1;
    background: var(--bg-input);
    border: 1px solid var(--border-strong);
    color: var(--text-bright);
    font-family: inherit;
    font-size: 0.78rem;
    padding: 0.45rem 0.6rem;
    letter-spacing: 0.02em;
    min-width: 0;
  }

  .field-input:focus {
    outline: none;
    border-color: var(--accent);
  }

  .field-input:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }

  .field-actions {
    margin-top: 0.85rem;
    display: flex;
    justify-content: flex-end;
  }

  .btn {
    background: transparent;
    border: 1px solid var(--border-strong);
    color: var(--text-soft);
    font-family: inherit;
    font-size: 0.75rem;
    padding: 0.45rem 0.8rem;
    cursor: pointer;
    letter-spacing: 0.05em;
    transition: all 0.14s;
    text-align: center;
    white-space: nowrap;
  }

  .btn:hover:not(:disabled) {
    color: var(--text-bright);
    border-color: var(--text-soft);
    background: var(--hover-tint);
  }

  .btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .btn-thin {
    flex-shrink: 0;
    padding: 0.45rem 0.7rem;
    font-size: 0.72rem;
  }

  .btn-detect {
    color: var(--accent);
    border-color: var(--accent);
    opacity: 0.75;
  }

  .btn-detect:hover:not(:disabled) {
    opacity: 1;
    background: var(--hover-tint);
  }

  .detect-panel {
    margin-top: 0.4rem;
    border: 1px dashed var(--border);
    background: var(--bg-hint);
    padding: 0.5rem 0.7rem;
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }

  .detect-status {
    font-size: 0.69rem;
    color: var(--text-muted);
    font-style: italic;
    letter-spacing: 0.06em;
    padding-bottom: 0.15rem;
  }

  .detect-item {
    background: transparent;
    border: none;
    border-bottom: 1px dashed var(--border);
    color: var(--text-soft);
    font-family: inherit;
    font-size: 0.74rem;
    padding: 0.3rem 0.2rem;
    cursor: pointer;
    text-align: left;
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 0.8rem;
    transition: all 0.12s;
    min-width: 0;
  }

  .detect-item:last-child {
    border-bottom: none;
  }

  .detect-item:hover {
    color: var(--text-bright);
    background: var(--hover-tint);
    padding-left: 0.5rem;
  }

  .detect-label {
    color: var(--text-faint);
    font-size: 0.67rem;
    flex-shrink: 0;
    letter-spacing: 0.04em;
  }

  .detect-path {
    color: var(--accent);
    font-size: 0.73rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
    text-align: right;
  }

  .btn-power.on {
    color: var(--success);
    border-color: var(--success-border);
  }

  .btn-power.on:hover {
    color: var(--success-bright);
    background: var(--success-glow-bg);
  }

  .btn-watch.active {
    color: var(--success);
    border-color: var(--success-border);
  }

  .btn-watch.active:hover {
    color: var(--success-bright);
    background: var(--success-glow-bg);
  }

  .proc-watch-row {
    display: flex;
    align-items: center;
    gap: 0.8rem;
    margin-top: 0.7rem;
    flex-wrap: wrap;
  }

  .proc-status {
    font-size: 0.7rem;
    color: var(--text-muted);
    font-style: italic;
    letter-spacing: 0.05em;
  }

  .proc-status.active {
    color: var(--success);
  }

  .ops .ops-row {
    display: flex;
    flex-wrap: wrap;
    gap: 0.55rem;
  }

  .ops .ops-row .btn {
    flex: 1;
    min-width: 130px;
  }

  .meta-row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 0.35rem 0;
    font-size: 0.78rem;
    border-bottom: 1px dotted var(--border);
  }

  .meta-row:last-child {
    border-bottom: none;
  }

  .meta-key {
    color: var(--text-muted);
    letter-spacing: 0.06em;
    text-transform: uppercase;
    font-size: 0.7rem;
  }

  .meta-val {
    color: var(--text-bright);
    font-variant-numeric: tabular-nums;
  }

  .meta-val.dim {
    color: var(--text-faint);
    font-style: italic;
  }

  .uuid-val {
    font-size: 0.68rem;
    letter-spacing: 0.04em;
    color: var(--accent);
  }

  .meta-row.error {
    background: var(--error-bg);
    margin: 0.3rem -1rem -0.3rem;
    padding: 0.45rem 1rem;
  }

  .meta-val.err {
    color: var(--error-text);
    font-size: 0.74rem;
    text-align: right;
    word-break: break-word;
  }

  .kind-toggle {
    display: flex;
    gap: 0;
    border: 1px solid var(--border-strong);
    width: fit-content;
  }

  .kind-opt {
    background: transparent;
    border: none;
    color: var(--text-muted);
    font-family: inherit;
    font-size: 0.74rem;
    padding: 0.4rem 0.9rem;
    cursor: pointer;
    letter-spacing: 0.05em;
    transition: all 0.14s;
  }

  .kind-opt + .kind-opt {
    border-left: 1px solid var(--border-strong);
  }

  .kind-opt:hover:not(:disabled):not(.active) {
    color: var(--text-bright);
    background: var(--hover-tint);
  }

  .kind-opt.active {
    color: var(--accent);
    background: var(--bg-hint);
  }

  .kind-opt:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .hint-line {
    margin: 0.3rem 0 0;
    font-size: 0.69rem;
    color: var(--text-faint);
    font-style: italic;
    letter-spacing: 0.04em;
    word-break: break-word;
  }

  .hint-line.err {
    color: var(--error-text, #e05c5c);
    font-style: normal;
  }

  .hist-row {
    display: flex;
    align-items: flex-start;
    gap: 0.8rem;
    margin-bottom: 0.6rem;
    flex-wrap: wrap;
  }

  .hist-row .field-label {
    margin: 0;
    min-width: 80px;
    padding-top: 0.3rem;
  }

  .check-stack {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    flex: 1;
    min-width: 0;
  }

  .check-row {
    display: flex;
    align-items: baseline;
    gap: 0.5rem;
    cursor: pointer;
    color: var(--text-bright);
    font-size: 0.78rem;
    letter-spacing: 0.04em;
    flex-wrap: wrap;
  }

  .check-row.disabled {
    color: var(--text-faint);
    cursor: not-allowed;
  }

  .check-row input[type="checkbox"] {
    accent-color: var(--accent);
    width: 1rem;
    height: 1rem;
    cursor: pointer;
    flex-shrink: 0;
  }

  .check-row input[type="checkbox"]:disabled {
    cursor: not-allowed;
    opacity: 0.5;
  }

  .check-label {
    font-family: "Major Mono Display", monospace;
    color: var(--accent);
    text-transform: lowercase;
  }

  .check-row.disabled .check-label {
    color: var(--text-faint);
  }

  .check-hint {
    color: var(--text-muted);
    font-size: 0.69rem;
    font-style: italic;
  }

  .field-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 0.7rem;
    margin-top: 0.6rem;
  }

  @media (max-width: 540px) {
    .field-grid {
      grid-template-columns: 1fr;
    }
  }
</style>
