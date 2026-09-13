<script lang="ts">
  import { goto } from "$app/navigation";
  import { page } from "$app/stores";
  import { api, ApiError } from "$lib/rpc";
  import { _ } from "svelte-i18n";

  let username = $state("");
  let password = $state("");
  let busy = $state(false);
  let err = $state("");

  /**
   * Mensagens de erro do login são deliberadamente vagas quanto a *qual*
   * parte falhou — o backend já responde igual pra usuário inexistente e
   * senha errada, e detalhar aqui desfaria isso.
   */
  function message(e: unknown): string {
    if (e instanceof ApiError) {
      if (e.code === "invalid_credentials") return $_("login.invalid");
      if (e.code === "account_locked") {
        return e.detail ? `${$_("login.locked")} (${e.detail})` : $_("login.locked");
      }
    }
    return $_("login.unreachable");
  }

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    if (busy) return;
    busy = true;
    err = "";
    try {
      await api.post("/login", { username, password });
      const next = $page.url.searchParams.get("next") || "/";
      // `next` vem da query string: aceitar só caminho relativo impede que
      // um link forjado use a tela de login como trampolim pra outro site.
      await goto(next.startsWith("/") && !next.startsWith("//") ? next : "/");
    } catch (e) {
      err = message(e);
      password = "";
    } finally {
      busy = false;
    }
  }
</script>

<div class="wrap">
  <form class="card" onsubmit={submit}>
    <div class="head">
      <span class="led led-green"></span>
      <span class="title">save-sync</span>
    </div>

    <label class="field">
      <span class="tag">{$_("login.username")}</span>
      <!-- svelte-ignore a11y_autofocus -->
      <input
        bind:value={username}
        autocomplete="username"
        autocapitalize="off"
        autocorrect="off"
        spellcheck="false"
        autofocus
        required
      />
    </label>

    <label class="field">
      <span class="tag">{$_("login.password")}</span>
      <input
        type="password"
        bind:value={password}
        autocomplete="current-password"
        required
      />
    </label>

    {#if err}
      <p class="err" role="alert">{err}</p>
    {/if}

    <button class="submit" type="submit" disabled={busy}>
      {busy ? $_("login.submitting") : $_("login.submit")}
    </button>

    <p class="hint">{$_("login.bootstrap_hint")}</p>
  </form>
</div>

<style>
  .wrap {
    min-height: 70vh;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 24px 16px;
  }

  .card {
    width: 100%;
    max-width: 360px;
    display: flex;
    flex-direction: column;
    gap: 14px;
    padding: 22px;
    background: var(--bg-elev);
    border: 1px solid var(--border);
  }

  .head {
    display: flex;
    align-items: center;
    gap: 8px;
    padding-bottom: 12px;
    border-bottom: 1px solid var(--border);
  }

  .title {
    color: var(--accent);
    letter-spacing: 0.12em;
    text-transform: uppercase;
    font-size: 0.95rem;
  }

  .led {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--ok, #4ade80);
    box-shadow: 0 0 6px var(--ok, #4ade80);
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: 5px;
  }

  .tag {
    font-size: 0.72rem;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    color: var(--text-dim);
  }

  input {
    background: var(--bg);
    border: 1px solid var(--border);
    color: var(--text);
    padding: 9px 10px;
    font: inherit;
    width: 100%;
    box-sizing: border-box;
  }

  input:focus {
    outline: none;
    border-color: var(--accent);
  }

  .submit {
    margin-top: 4px;
    padding: 10px;
    background: transparent;
    border: 1px solid var(--accent);
    color: var(--accent);
    font: inherit;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    cursor: pointer;
  }

  .submit:hover:not(:disabled) {
    background: var(--accent);
    color: var(--bg);
  }

  .submit:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .err {
    margin: 0;
    color: var(--err, #f87171);
    font-size: 0.82rem;
  }

  .hint {
    margin: 0;
    padding-top: 10px;
    border-top: 1px solid var(--border);
    color: var(--text-dim);
    font-size: 0.72rem;
    line-height: 1.5;
  }
</style>
