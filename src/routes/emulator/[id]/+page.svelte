<script lang="ts">
  import { goto } from "$app/navigation";
  import { page } from "$app/stores";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { open } from "@tauri-apps/plugin-dialog";
  import { emulators, type EmulatorView } from "$lib/store";
  import { derived } from "svelte/store";
  import { onMount } from "svelte";
  import { _ } from "svelte-i18n";
  import { tErr } from "$lib/i18n";

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

  interface PruneSummary {
    deleted_count: number;
    freed_bytes: number;
  }
  let pruning = $state(false);
  let pruneMsg = $state("");
  let pruneErr = $state("");

  interface ConflictEntry {
    path: string;
    conflict_path: string;
    conflict_num: number;
    current_size: number;
    conflict_size: number;
    current_modified: string;
    conflict_modified: string;
  }
  let conflicts = $state<ConflictEntry[]>([]);
  let conflictsLoading = $state(false);
  let conflictsErr = $state("");
  let resolvingPath = $state<string | null>(null);

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
      conflicts = [];
      conflictsErr = "";
      if (emu.id === "eden" && emu.source_path) {
        refreshEdenUuid(emu.source_path);
      }
      loadHistorySettings(emu.id);
      // Only attempt conflict listing if dest is configured — otherwise
      // backend errors out and we surface a noisy validation message.
      if (emu.dest_path) loadConflicts(emu.id);
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
      historyErr = tErr(e);
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
      historyErr = tErr(e);
    } finally {
      savingHistory = false;
    }
  }

  async function pruneNow(emuId: string) {
    pruning = true;
    pruneMsg = "";
    pruneErr = "";
    try {
      const r = await invoke<PruneSummary>("prune_history_now", { id: emuId });
      const mb = (r.freed_bytes / 1024 / 1024).toFixed(1);
      if (r.deleted_count === 0) {
        pruneMsg = $_("emulator.history.prune_summary_empty");
      } else {
        pruneMsg = $_("emulator.history.prune_summary_done", {
          values: { n: r.deleted_count, mb },
        });
      }
      // Auto-clear after a few seconds.
      setTimeout(() => (pruneMsg = ""), 6000);
    } catch (e) {
      pruneErr = tErr(e);
    } finally {
      pruning = false;
    }
  }

  async function loadConflicts(emuId: string) {
    conflictsLoading = true;
    conflictsErr = "";
    try {
      conflicts = await invoke<ConflictEntry[]>("list_conflicts", { id: emuId });
    } catch (e) {
      conflictsErr = tErr(e);
      conflicts = [];
    } finally {
      conflictsLoading = false;
    }
  }

  async function resolveConflict(emuId: string, c: ConflictEntry, action: string) {
    resolvingPath = c.conflict_path;
    conflictsErr = "";
    try {
      await invoke("resolve_conflict", {
        id: emuId,
        conflictPath: c.conflict_path,
        action,
      });
      // Optimistic: remove resolved row immediately rather than refetching.
      conflicts = conflicts.filter((x) => x.conflict_path !== c.conflict_path);
    } catch (e) {
      conflictsErr = tErr(e);
    } finally {
      resolvingPath = null;
    }
  }

  function fmtBytes(b: number): string {
    if (b < 1024) return `${b} B`;
    if (b < 1024 * 1024) return `${(b / 1024).toFixed(1)} KB`;
    return `${(b / 1024 / 1024).toFixed(1)} MB`;
  }

  function fmtMTime(iso: string): string {
    if (!iso) return "—";
    const d = new Date(iso);
    if (isNaN(d.getTime())) return iso;
    return d.toLocaleString("pt-BR", {
      day: "2-digit",
      month: "2-digit",
      year: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
  }

  /** Compare two ISO strings, return "current" / "conflict" / "tie" / "" */
  function newerSide(c: ConflictEntry): "current" | "conflict" | "tie" | "" {
    if (!c.current_modified || !c.conflict_modified) return "";
    const a = new Date(c.current_modified).getTime();
    const b = new Date(c.conflict_modified).getTime();
    if (isNaN(a) || isNaN(b)) return "";
    if (a > b) return "current";
    if (b > a) return "conflict";
    return "tie";
  }

  async function loadRemotes() {
    remotesErr = "";
    try {
      availableRemotes = (await invoke<string[]>("rclone_list_remotes")).map((r) =>
        r.replace(/:$/, ""),
      );
    } catch (e) {
      remotesErr = tErr(e);
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
      debugMsg = "pickFolder: " + tErr(err);
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
      debugMsg = "set_emulator_paths: " + tErr(err);
    } finally {
      savingPaths = false;
    }
  }

  async function syncNow(emu: EmulatorView) {
    debugMsg = "";
    try {
      await invoke("sync_now", { id: emu.id });
    } catch (err) {
      debugMsg = "sync_now: " + tErr(err);
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
      debugMsg = "watch: " + tErr(err);
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
      debugMsg = "proc_watch: " + tErr(err);
    }
  }

  async function saveProcName(emu: EmulatorView) {
    debugMsg = "";
    savingProcName = true;
    try {
      await invoke("set_process_name", { id: emu.id, processName: procNameDraft });
    } catch (err) {
      debugMsg = "set_process_name: " + tErr(err);
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
      debugMsg = "set_enabled: " + tErr(err);
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
      debugMsg = "detect: " + tErr(err);
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
      titleDbErr = tErr(e);
    }
  }

  async function refreshTitleDb() {
    titleDbErr = "";
    try {
      await invoke("refresh_title_db");
      await loadTitleDbStatus();
    } catch (e) {
      titleDbErr = tErr(e);
      await loadTitleDbStatus();
    }
  }

  async function loadPs2DbStatus() {
    try {
      ps2Db = await invoke<TitleDbStatus>("ps2_db_status");
    } catch (e) {
      ps2DbErr = tErr(e);
    }
  }

  async function refreshPs2Db() {
    ps2DbErr = "";
    try {
      await invoke("refresh_ps2_db");
      await loadPs2DbStatus();
    } catch (e) {
      ps2DbErr = tErr(e);
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
  <button class="back" onclick={back} aria-label={$_("common.back")}>
    <span class="back-arrow">◀</span> {$_("common.back")}
  </button>
</section>

{#if !$current}
  <section class="empty">
    <p>{$_("emulator.unit_not_found")}<button class="link" onclick={back}>{$_("emulator.return")}</button></p>
  </section>
{:else}
  {@const emu = $current}

  <section class="head">
    <div class="head-row">
      <span class="led" class:led-green={emu.watching} class:led-amber={emu.enabled && !emu.watching} class:led-off={!emu.enabled}></span>
      <h1>{emu.name}</h1>
      <span class="state-tag">
        {#if !emu.enabled}
          {$_("emulator.state_disabled")}
        {:else if emu.watching}
          {$_("emulator.state_watching")}
        {:else}
          {$_("emulator.state_idle")}
        {/if}
      </span>
    </div>
    <p class="head-id">{$_("emulator.unit_id_label", { values: { id: emu.id } })}</p>
  </section>

  <section class="card ops">
    <header class="card-head">
      <span class="card-tag">{$_("emulator.ops.tag")}</span>
      <span class="card-meta">{$_("emulator.ops.subtitle")}</span>
    </header>

    <div class="ops-row">
      <button
        class="btn btn-power"
        class:on={emu.enabled}
        onclick={() => toggleEnabled(emu)}
      >
        {emu.enabled ? $_("emulator.ops.disable_unit") : $_("emulator.ops.enable_unit")}
      </button>

      <button class="btn" onclick={() => viewSaves(emu)}>
        {$_("emulator.ops.view_saves")}
      </button>

      <button
        class="btn"
        onclick={() => syncNow(emu)}
        disabled={!emu.enabled}
      >
        {$_("emulator.ops.sync_now")}
      </button>

      <button
        class="btn btn-watch"
        class:active={emu.watching}
        onclick={() => toggleWatch(emu)}
        disabled={!emu.enabled}
      >
        {emu.watching ? $_("emulator.ops.halt_watcher") : $_("emulator.ops.engage_watcher")}
      </button>
    </div>
  </section>

  {#if debugMsg}
    <section class="alert">
      <span class="alert-tag">{$_("common.error_tag")}</span>
      <span>{debugMsg}</span>
    </section>
  {/if}

  <section class="card">
    <header class="card-head">
      <span class="card-tag">{$_("emulator.hint.tag")}</span>
      <span class="card-meta">{$_("emulator.hint.subtitle")}</span>
    </header>
    <p class="hint">{emu.hint}</p>
  </section>

  <section class="card">
    <header class="card-head">
      <span class="card-tag">{$_("emulator.paths.tag")}</span>
      <span class="card-meta">{$_("emulator.paths.subtitle")}</span>
    </header>

    <div class="field">
      <label class="field-label" for="source-path">{$_("emulator.paths.source_label")}</label>
      <div class="field-row">
        <input
          id="source-path"
          type="text"
          class="field-input"
          bind:value={sourceDraft}
          placeholder={$_("emulator.paths.placeholder_source")}
          disabled={!emu.enabled}
        />
        <button class="btn btn-thin" onclick={() => pickFolder("source")} disabled={!emu.enabled}>
          {$_("emulator.paths.browse_btn")}
        </button>
        <button
          class="btn btn-thin btn-detect"
          onclick={() => detectPaths(emu)}
          disabled={!emu.enabled || detecting}
        >
          {detecting ? "…" : $_("emulator.paths.detect_btn")}
        </button>
      </div>

      {#if detectDone || detecting}
        <div class="detect-panel">
          {#if detecting}
            <span class="detect-status">{$_("emulator.paths.detect_status_scanning")}</span>
          {:else if detectCandidates.length === 0}
            <span class="detect-status">{$_("emulator.paths.detect_status_none")}</span>
          {:else}
            <span class="detect-status">
              {detectCandidates.length === 1
                ? $_("emulator.paths.detect_status_found_singular", { values: { n: detectCandidates.length } })
                : $_("emulator.paths.detect_status_found_plural", { values: { n: detectCandidates.length } })}
            </span>
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
      <span class="field-label">{$_("emulator.paths.dest_kind_label")}</span>
      <div class="kind-toggle">
        <button
          type="button"
          class="kind-opt"
          class:active={destKindDraft === "local"}
          onclick={() => (destKindDraft = "local")}
          disabled={!emu.enabled}
        >
          {$_("emulator.paths.kind_local")}
        </button>
        <button
          type="button"
          class="kind-opt"
          class:active={destKindDraft === "rclone"}
          onclick={() => (destKindDraft = "rclone")}
          disabled={!emu.enabled}
        >
          {$_("emulator.paths.kind_rclone")}
        </button>
      </div>
    </div>

    {#if destKindDraft === "rclone"}
      <div class="field">
        <label class="field-label" for="dest-remote">{$_("emulator.paths.remote_label")}</label>
        <div class="field-row">
          <select
            id="dest-remote"
            class="field-input"
            bind:value={destRemoteDraft}
            disabled={!emu.enabled}
          >
            <option value="" disabled>{$_("emulator.paths.remote_select_placeholder")}</option>
            {#each availableRemotes as r (r)}
              <option value={r}>{r}</option>
            {/each}
          </select>
          <button
            class="btn btn-thin"
            onclick={() => goto("/remotes")}
            disabled={!emu.enabled}
          >
            {$_("emulator.paths.manage_btn")}
          </button>
        </div>
        {#if remotesErr}
          <p class="hint-line err">! {remotesErr}</p>
        {:else if availableRemotes.length === 0}
          <p class="hint-line">{$_("emulator.paths.remote_none_hint")}</p>
        {/if}
      </div>

      <div class="field">
        <label class="field-label" for="dest-path">{$_("emulator.paths.dest_remote_path_label")}</label>
        <div class="field-row">
          <input
            id="dest-path"
            type="text"
            class="field-input"
            bind:value={destDraft}
            placeholder={$_("emulator.paths.placeholder_dest_remote")}
            disabled={!emu.enabled}
          />
        </div>
        <p class="hint-line">
          {$_("emulator.paths.dest_remote_path_preview", {
            values: {
              remote: destRemoteDraft || "<remote>",
              path: destDraft || "<path>",
              id: emu.id,
            },
          })}
        </p>
      </div>
    {:else}
      <div class="field">
        <label class="field-label" for="dest-path">{$_("emulator.paths.dest_local_label")}</label>
        <div class="field-row">
          <input
            id="dest-path"
            type="text"
            class="field-input"
            bind:value={destDraft}
            placeholder={$_("emulator.paths.placeholder_dest_local")}
            disabled={!emu.enabled}
          />
          <button class="btn btn-thin" onclick={() => pickFolder("dest")} disabled={!emu.enabled}>
            {$_("emulator.paths.browse_btn")}
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
        {savingPaths ? $_("common.saving") + "..." : pathDirty(emu) ? $_("emulator.paths.commit_btn") : $_("emulator.paths.saved_btn")}
      </button>
    </div>
  </section>

  <section class="card">
    <header class="card-head">
      <span class="card-tag">{$_("emulator.status.tag")}</span>
      <span class="card-meta">{$_("emulator.status.subtitle")}</span>
    </header>

    {#if emu.id === "eden"}
      <div class="meta-row">
        <span class="meta-key">{$_("emulator.status.profile_uuid")}</span>
        <span class="meta-val uuid-val" class:dim={!edenUuid}>
          {edenUuid ?? (emu.source_path ? $_("emulator.status.uuid_not_detected") : $_("emulator.status.uuid_no_source"))}
        </span>
      </div>
    {/if}

    <div class="meta-row">
      <span class="meta-key">{$_("emulator.status.last_sync")}</span>
      <span class="meta-val" class:dim={!emu.last_sync}>
        {emu.last_sync ?? $_("common.never")}
      </span>
    </div>

    {#if emu.last_error}
      <div class="meta-row error">
        <span class="meta-key">{$_("emulator.status.last_error")}</span>
        <span class="meta-val err">{tErr(emu.last_error)}</span>
      </div>
    {/if}
  </section>

  <section class="card">
    <header class="card-head">
      <span class="card-tag">{$_("emulator.proc_watch.tag")}</span>
      <span class="card-meta">{$_("emulator.proc_watch.subtitle")}</span>
    </header>

    <div class="field">
      <label class="field-label" for="proc-name">{$_("emulator.proc_watch.process_label")}</label>
      <div class="field-row">
        <input
          id="proc-name"
          type="text"
          class="field-input"
          bind:value={procNameDraft}
          placeholder={$_("emulator.proc_watch.process_placeholder")}
          disabled={!emu.enabled}
        />
        <button
          class="btn btn-thin"
          onclick={() => saveProcName(emu)}
          disabled={!emu.enabled || savingProcName || !procNameDirty(emu)}
        >
          {savingProcName ? $_("common.saving") + "..." : procNameDirty(emu) ? $_("emulator.proc_watch.commit_btn") : $_("emulator.proc_watch.saved_btn")}
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
        {emu.proc_watching ? $_("emulator.proc_watch.halt_btn") : $_("emulator.proc_watch.engage_btn")}
      </button>
      <span class="proc-status" class:active={emu.proc_watching}>
        {emu.proc_watching ? $_("emulator.proc_watch.monitoring_msg") : $_("emulator.proc_watch.idle_msg")}
      </span>
    </div>
  </section>

  <section class="card" class:has-conflicts={conflicts.length > 0}>
    <header class="card-head">
      <span class="card-tag" class:warn={conflicts.length > 0}>{$_("emulator.conflicts.tag")}</span>
      <span class="card-meta">
        {#if conflictsLoading}
          {$_("emulator.conflicts.scanning")}
        {:else if conflicts.length === 0}
          {$_("common.none")}
        {:else}
          {$_("emulator.conflicts.count_unresolved", { values: { n: conflicts.length } })}
        {/if}
      </span>
      <button
        class="hist-refresh"
        onclick={() => loadConflicts(emu.id)}
        disabled={conflictsLoading || !emu.dest_path}
        aria-label={$_("common.refresh")}
      >↻</button>
    </header>

    {#if conflictsErr}
      <p class="conflict-err">! {conflictsErr}</p>
    {:else if !conflictsLoading && conflicts.length === 0}
      <p class="conflict-empty">{$_("emulator.conflicts.none")}</p>
    {:else if conflicts.length > 0}
      <p class="conflict-hint">
        {conflicts.length === 1
          ? $_("emulator.conflicts.hint_singular", { values: { n: conflicts[0]?.conflict_num ?? 1 } })
          : $_("emulator.conflicts.hint_plural", { values: { count: conflicts.length, n: conflicts[0]?.conflict_num ?? 1 } })}
      </p>
      <ul class="conflict-list">
        {#each conflicts as c (c.conflict_path)}
          {@const newer = newerSide(c)}
          <li class="conflict-row" class:resolving={resolvingPath === c.conflict_path}>
            <div class="conflict-path-row">
              <span class="conflict-path-text" title={c.path}>{c.path}</span>
              <span class="conflict-suffix">.conflict{c.conflict_num}</span>
            </div>
            <div class="conflict-sides">
              <div class="conflict-side" class:newer={newer === "current"}>
                <span class="side-label">{$_("emulator.conflicts.side_current")}</span>
                <span class="side-detail">{fmtBytes(c.current_size)} · {fmtMTime(c.current_modified)}</span>
                {#if newer === "current"}<span class="newer-tag">{$_("emulator.conflicts.tag_newer")}</span>{/if}
              </div>
              <div class="conflict-side" class:newer={newer === "conflict"}>
                <span class="side-label">{$_("emulator.conflicts.side_conflict")}</span>
                <span class="side-detail">{fmtBytes(c.conflict_size)} · {fmtMTime(c.conflict_modified)}</span>
                {#if newer === "conflict"}<span class="newer-tag">{$_("emulator.conflicts.tag_newer")}</span>{/if}
              </div>
            </div>
            <div class="conflict-actions">
              <button
                class="btn btn-thin"
                onclick={() => resolveConflict(emu.id, c, "keep_current")}
                disabled={resolvingPath !== null}
                title={$_("emulator.conflicts.tooltip_keep_current")}
              >{$_("emulator.conflicts.btn_keep_current")}</button>
              <button
                class="btn btn-thin"
                onclick={() => resolveConflict(emu.id, c, "use_conflict")}
                disabled={resolvingPath !== null}
                title={$_("emulator.conflicts.tooltip_use_conflict")}
              >{$_("emulator.conflicts.btn_use_conflict")}</button>
              <button
                class="btn btn-thin"
                onclick={() => resolveConflict(emu.id, c, "keep_both")}
                disabled={resolvingPath !== null}
                title={$_("emulator.conflicts.tooltip_keep_both")}
              >{$_("emulator.conflicts.btn_keep_both")}</button>
            </div>
          </li>
        {/each}
      </ul>
    {/if}
  </section>

  <section class="card">
    <header class="card-head">
      <span class="card-tag">{$_("emulator.history.tag")}</span>
      <span class="card-meta">{$_("emulator.history.subtitle")}</span>
    </header>

    {#if !historyDraft}
      <p class="hint-line">// {$_("common.loading")}…</p>
    {:else}
      <div class="hist-row">
        <span class="field-label">{$_("emulator.history.backup_label")}</span>
        <div class="kind-toggle">
          <button
            type="button"
            class="kind-opt"
            class:active={historyDraft.enabled}
            onclick={() => (historyDraft!.enabled = true)}
          >
            {$_("home.btn_on")}
          </button>
          <button
            type="button"
            class="kind-opt"
            class:active={!historyDraft.enabled}
            onclick={() => (historyDraft!.enabled = false)}
          >
            {$_("home.btn_off")}
          </button>
        </div>
      </div>

      <div class="hist-row">
        <span class="field-label">{$_("emulator.history.mode_label")}</span>
        <div class="check-stack">
          <label
            class="check-row"
            class:on={historyDraft.incremental_enabled}
            class:disabled={!allowsIncremental || !historyDraft.enabled}
          >
            <input
              type="checkbox"
              bind:checked={historyDraft.incremental_enabled}
              disabled={!allowsIncremental || !historyDraft.enabled || lockOff("incremental")}
            />
            <span class="check-mark" aria-hidden="true">
              {historyDraft.incremental_enabled ? "[■]" : "[ ]"}
            </span>
            <div class="check-text">
              <span class="check-label">{$_("emulator.history.mode_incremental_label")}</span>
              <span class="check-hint">{$_("emulator.history.mode_incremental_hint")}</span>
            </div>
          </label>
          <label
            class="check-row"
            class:on={historyDraft.full_enabled}
            class:disabled={!historyDraft.enabled}
          >
            <input
              type="checkbox"
              bind:checked={historyDraft.full_enabled}
              disabled={!historyDraft.enabled || lockOff("full")}
            />
            <span class="check-mark" aria-hidden="true">
              {historyDraft.full_enabled ? "[■]" : "[ ]"}
            </span>
            <div class="check-text">
              <span class="check-label">{$_("emulator.history.mode_full_label")}</span>
              <span class="check-hint">{$_("emulator.history.mode_full_hint")}</span>
            </div>
          </label>
        </div>
      </div>

      {#if !allowsIncremental}
        <p class="hint-line">{$_("emulator.history.hint_file_based", { values: { id: emu.id } })}</p>
      {:else if historyDraft.incremental_enabled && historyDraft.full_enabled}
        <p class="hint-line">{$_("emulator.history.hint_both")}</p>
      {:else if historyDraft.incremental_enabled}
        <p class="hint-line">{$_("emulator.history.hint_incremental")}</p>
      {:else if historyDraft.full_enabled}
        <p class="hint-line">{$_("emulator.history.hint_full")}</p>
      {/if}
      {#if historyInvalid()}
        <p class="hint-line err">{$_("emulator.history.hint_invalid")}</p>
      {/if}

      <div class="field-grid">
        <div class="field">
          <label class="field-label" for="ret-days">{$_("emulator.history.retention_days_label")}</label>
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
          <label class="field-label" for="ret-mb">{$_("emulator.history.retention_mb_label")}</label>
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
        <span class="meta-key">{$_("emulator.history.bisync_state_label")}</span>
        <span class="meta-val" class:dim={!historyDraft.bisync_initialized}>
          {historyDraft.bisync_initialized ? $_("emulator.history.bisync_initialized") : $_("emulator.history.bisync_needs_resync")}
        </span>
      </div>

      {#if historyErr}
        <div class="meta-row error">
          <span class="meta-key">{$_("emulator.status.last_error")}</span>
          <span class="meta-val err">{historyErr}</span>
        </div>
      {/if}

      {#if pruneMsg}
        <p class="hint-line" style="color: var(--success, #5ec07a); font-style: normal;">
          {pruneMsg}
        </p>
      {/if}
      {#if pruneErr}
        <p class="hint-line err">! {pruneErr}</p>
      {/if}

      <div class="field-actions">
        <button
          class="btn btn-thin"
          onclick={() => pruneNow(emu.id)}
          disabled={pruning || !emu.dest_path}
          title={$_("emulator.history.prune_tooltip")}
        >
          {pruning ? $_("emulator.history.pruning_btn") : $_("emulator.history.prune_now_btn")}
        </button>
        <button
          class="btn"
          onclick={saveHistory}
          disabled={savingHistory || !historyDirty() || historyInvalid()}
        >
          {savingHistory ? $_("common.saving") + "..." : historyDirty() ? $_("emulator.history.commit_btn") : $_("emulator.history.saved_btn")}
        </button>
      </div>
    {/if}
  </section>

  {#if emu.id === "eden"}
    <section class="card">
      <header class="card-head">
        <span class="card-tag">{$_("emulator.titledb.tag")}</span>
        <span class="card-meta">{$_("emulator.titledb.subtitle")}</span>
      </header>

      <div class="meta-row">
        <span class="meta-key">{$_("emulator.titledb.entries")}</span>
        <span class="meta-val" class:dim={!titleDb || titleDb.count === 0}>
          {titleDb ? titleDb.count.toLocaleString() : "…"}
        </span>
      </div>
      <div class="meta-row">
        <span class="meta-key">{$_("emulator.titledb.last_update")}</span>
        <span class="meta-val" class:dim={!titleDb?.last_update}>
          {titleDb?.last_update ?? $_("common.never")}
        </span>
      </div>
      {#if titleDbErr}
        <div class="meta-row error">
          <span class="meta-key">{$_("emulator.status.last_error")}</span>
          <span class="meta-val err">{titleDbErr}</span>
        </div>
      {/if}

      <div class="field-actions">
        <button
          class="btn"
          onclick={refreshTitleDb}
          disabled={titleDb?.refreshing}
        >
          {titleDb?.refreshing ? $_("emulator.titledb.downloading") : $_("emulator.titledb.refresh_btn")}
        </button>
      </div>
    </section>
  {/if}

  {#if emu.id === "pcsx2"}
    <section class="card">
      <header class="card-head">
        <span class="card-tag">{$_("emulator.ps2db.tag")}</span>
        <span class="card-meta">{$_("emulator.ps2db.subtitle")}</span>
      </header>

      <div class="meta-row">
        <span class="meta-key">{$_("emulator.titledb.entries")}</span>
        <span class="meta-val" class:dim={!ps2Db || ps2Db.count === 0}>
          {ps2Db ? ps2Db.count.toLocaleString() : "…"}
        </span>
      </div>
      <div class="meta-row">
        <span class="meta-key">{$_("emulator.titledb.last_update")}</span>
        <span class="meta-val" class:dim={!ps2Db?.last_update}>
          {ps2Db?.last_update ?? $_("common.never")}
        </span>
      </div>
      {#if ps2DbErr}
        <div class="meta-row error">
          <span class="meta-key">{$_("emulator.status.last_error")}</span>
          <span class="meta-val err">{ps2DbErr}</span>
        </div>
      {/if}

      <div class="field-actions">
        <button
          class="btn"
          onclick={refreshPs2Db}
          disabled={ps2Db?.refreshing}
        >
          {ps2Db?.refreshing ? $_("emulator.titledb.downloading") : $_("emulator.ps2db.refresh_btn")}
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
    gap: 0.5rem;
    flex-wrap: wrap;
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

  .card.has-conflicts {
    border-color: var(--accent);
    box-shadow: 0 0 0 1px var(--accent);
  }

  .card-tag.warn {
    color: var(--error-text, #e05c5c);
    animation: pulse 1.6s ease-in-out infinite;
  }

  @keyframes pulse {
    0%, 100% { opacity: 1; }
    50%      { opacity: 0.55; }
  }

  .hist-refresh {
    background: transparent;
    border: 1px dashed var(--border-strong);
    color: var(--text-muted);
    font-family: inherit;
    font-size: 0.85rem;
    padding: 0.1rem 0.45rem;
    cursor: pointer;
    transition: all 0.14s;
    margin-left: 0.5rem;
  }

  .hist-refresh:hover:not(:disabled) {
    color: var(--accent);
    border-color: var(--accent);
  }

  .hist-refresh:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .conflict-empty {
    margin: 0.3rem 0;
    font-size: 0.74rem;
    color: var(--text-muted);
    font-style: italic;
    letter-spacing: 0.04em;
  }

  .conflict-err {
    margin: 0.3rem 0;
    font-size: 0.76rem;
    color: var(--error-text, #e05c5c);
  }

  .conflict-hint {
    margin: 0 0 0.6rem;
    font-size: 0.7rem;
    color: var(--text-faint);
    font-style: italic;
    letter-spacing: 0.04em;
  }

  .conflict-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.7rem;
  }

  .conflict-row {
    border: 1px dashed var(--border);
    padding: 0.6rem 0.7rem;
    background: var(--bg-hint);
    display: flex;
    flex-direction: column;
    gap: 0.45rem;
  }

  .conflict-row.resolving {
    opacity: 0.55;
  }

  .conflict-path-row {
    display: flex;
    align-items: baseline;
    gap: 0.4rem;
    min-width: 0;
  }

  .conflict-path-text {
    color: var(--text-bright);
    font-size: 0.78rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
    flex: 1;
  }

  .conflict-suffix {
    color: var(--accent);
    font-size: 0.7rem;
    font-family: "Major Mono Display", monospace;
    flex-shrink: 0;
  }

  .conflict-sides {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 0.5rem;
  }

  @media (max-width: 540px) {
    .conflict-sides {
      grid-template-columns: 1fr;
    }
  }

  .conflict-side {
    border-left: 2px solid var(--border-strong);
    padding: 0.3rem 0.5rem;
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
    position: relative;
  }

  .conflict-side.newer {
    border-left-color: var(--accent);
  }

  .side-label {
    color: var(--text-muted);
    font-size: 0.66rem;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }

  .side-detail {
    color: var(--text-soft);
    font-size: 0.72rem;
    font-variant-numeric: tabular-nums;
  }

  .newer-tag {
    position: absolute;
    top: 0.25rem;
    right: 0.4rem;
    color: var(--accent);
    font-size: 0.62rem;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }

  .conflict-actions {
    display: flex;
    gap: 0.45rem;
    flex-wrap: wrap;
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

  .check-row {
    display: flex;
    align-items: flex-start;
    gap: 0.55rem;
    cursor: pointer;
    padding: 0.35rem 0.5rem;
    border: 1px dashed transparent;
    transition: background 0.12s, border-color 0.12s;
  }

  .check-row:hover:not(.disabled) {
    background: var(--hover-tint);
    border-color: var(--border);
  }

  .check-row.disabled {
    cursor: not-allowed;
  }

  /* Native input is the source-of-truth for the form state but visually
     hidden — the `.check-mark` span renders the ASCII checkbox instead.
     Kept reachable via keyboard (Tab + Space) by leaving it in the DOM. */
  .check-row input[type="checkbox"] {
    position: absolute;
    width: 1px;
    height: 1px;
    opacity: 0;
    pointer-events: none;
    margin: 0;
  }

  .check-mark {
    font-family: "Major Mono Display", monospace;
    font-size: 0.95rem;
    color: var(--text-muted);
    letter-spacing: -0.04em;
    flex-shrink: 0;
    transition: color 0.12s, text-shadow 0.12s;
    line-height: 1.15;
    user-select: none;
    /* Fixed width so [ ] and [■] occupy the same horizontal space —
       prevents the label from jittering when toggling. */
    min-width: 1.9rem;
    text-align: center;
  }

  .check-row.on .check-mark {
    color: var(--accent);
    text-shadow: 0 0 6px rgba(255, 191, 0, 0.35);
  }

  .check-row:hover:not(.disabled) .check-mark {
    color: var(--accent);
  }

  .check-row.disabled .check-mark {
    opacity: 0.4;
  }

  /* Keyboard focus indicator — outline the bracket since the input is hidden. */
  .check-row input[type="checkbox"]:focus-visible + .check-mark {
    outline: 1px dashed var(--accent);
    outline-offset: 2px;
  }

  .check-text {
    display: flex;
    flex-direction: column;
    gap: 0.1rem;
    min-width: 0;
    padding-top: 0.05rem;
  }

  .check-label {
    color: var(--text-bright);
    font-size: 0.82rem;
    letter-spacing: 0.05em;
    text-transform: lowercase;
  }

  .check-row.on .check-label {
    color: var(--accent);
  }

  .check-row.disabled .check-label {
    color: var(--text-faint);
  }

  .check-hint {
    color: var(--text-muted);
    font-size: 0.68rem;
    font-style: italic;
    letter-spacing: 0.04em;
    line-height: 1.35;
  }

  .check-stack {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    flex: 1;
    min-width: 0;
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
