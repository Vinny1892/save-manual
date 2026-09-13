/**
 * Transporte único da UI.
 *
 * A mesma SPA roda em dois lugares: dentro da janela do Tauri (client de PC)
 * e num browser qualquer apontado pro server do NAS. Este módulo esconde a
 * diferença atrás de um `invoke()` com a mesma assinatura que o do Tauri,
 * então as páginas não sabem onde estão rodando.
 *
 * O roteamento é por nome de comando porque a divisão não é por tela, é por
 * natureza da operação:
 *
 *   - **local**: precisa da máquina onde o emulador está — varrer disco,
 *     observar processo, abrir pasta no explorador. Só existe no Tauri.
 *   - **server**: estado autoritativo — o que está sincronizado, histórico,
 *     conflitos, política de retenção. Existe nos dois (no Tauri também é
 *     HTTP, contra o mesmo server).
 *
 * Comando local chamado do browser lança `BrowserUnsupported`, que a UI usa
 * pra esconder o controle em vez de mostrar erro.
 */

import { invoke as tauriInvoke } from "@tauri-apps/api/core";

export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/** Erro de comando que só existe no client de PC. */
export class BrowserUnsupported extends Error {
  constructor(public command: string) {
    super(`comando '${command}' não existe no browser`);
    this.name = "BrowserUnsupported";
  }
}

/**
 * Base do server. No browser é a própria origem — a SPA é servida por ele.
 * No Tauri vem do que o usuário configurou ao parear; sem isso, as chamadas
 * de server falham e a UI mostra "não pareado".
 */
export function serverBase(): string {
  if (!isTauri()) return "";
  try {
    return localStorage.getItem("save-sync-server") ?? "";
  } catch {
    return "";
  }
}

export function setServerBase(url: string) {
  try {
    localStorage.setItem("save-sync-server", url.replace(/\/+$/, ""));
  } catch {
    /* modo privado: segue sem persistir */
  }
}

/**
 * Token do device, guardado ao parear. É a prova que o client de PC usa pra
 * ler do server: dentro do Tauri a origem é `tauri://localhost` e o cookie
 * de sessão não viajaria até o NAS sem `SameSite=None; Secure`, que exige
 * TLS. No browser não existe token — lá quem prova é o cookie.
 */
export function deviceToken(): string | null {
  if (!isTauri()) return null;
  try {
    return localStorage.getItem("save-sync-device-token");
  } catch {
    return null;
  }
}

export function setDeviceToken(token: string) {
  try {
    localStorage.setItem("save-sync-device-token", token);
  } catch {
    /* modo privado: segue sem persistir */
  }
}

/** Erro com o código estável do backend, pro `tErr()` traduzir. */
export class ApiError extends Error {
  constructor(
    public code: string,
    public status: number,
    public detail?: string,
  ) {
    super(code);
    this.name = "ApiError";
  }
}

async function http<T>(
  method: string,
  path: string,
  body?: unknown,
): Promise<T> {
  const headers: Record<string, string> = {};
  if (body !== undefined) headers["content-type"] = "application/json";
  const token = deviceToken();
  if (token) headers["authorization"] = `Bearer ${token}`;

  const res = await fetch(`${serverBase()}/api/v1${path}`, {
    method,
    // O cookie de sessão é HttpOnly; sem isto ele não acompanha a chamada.
    credentials: "include",
    headers,
    body: body === undefined ? undefined : JSON.stringify(body),
  });

  if (!res.ok) {
    let code = `http_${res.status}`;
    let detail: string | undefined;
    try {
      const parsed = await res.json();
      if (parsed?.error) code = parsed.error;
      detail = parsed?.detail;
    } catch {
      /* resposta sem corpo JSON: fica o código genérico */
    }
    throw new ApiError(code, res.status, detail);
  }

  if (res.status === 204) return undefined as T;
  return (await res.json()) as T;
}

export const api = {
  get: <T>(path: string) => http<T>("GET", path),
  post: <T>(path: string, body?: unknown) => http<T>("POST", path, body),
  put: <T>(path: string, body?: unknown) => http<T>("PUT", path, body),
  del: <T>(path: string) => http<T>("DELETE", path),
};

type Args = Record<string, any> | undefined;

/**
 * Comandos que o server atende. O que não está aqui só roda no Tauri.
 *
 * Os nomes são os mesmos de antes de propósito: as páginas continuam
 * chamando `invoke("list_saves", ...)` e a migração fica em uma linha de
 * import por arquivo.
 */
const SERVER_ROUTES: Record<string, (a: Args) => Promise<any>> = {
  list_emulators: async () => {
    const r = await api.get<{ emulators: any[] }>("/emulators");
    return r.emulators;
  },
  list_saves: async (a) => {
    const r = await api.get<{ saves: any[] }>(`/emulators/${a!.id}/saves`);
    return r.saves;
  },
  get_save_entry: (a) =>
    api.get(`/emulators/${a!.id}/saves/${encodeURIComponent(a!.raw_id)}`),
  get_history_settings: (a) => api.get(`/emulators/${a!.id}/settings`),
  set_history_settings: (a) =>
    api.put(`/emulators/${a!.settings.emulator_id}/settings`, a!.settings),
  list_conflicts: async (a) => {
    const r = await api.get<{ conflicts: any[] }>(
      `/emulators/${a!.id}/conflicts`,
    );
    return r.conflicts;
  },
};

/**
 * Substituto do `invoke` do Tauri.
 *
 * Dentro do Tauri, comando de server ainda vai por HTTP: o client é só mais
 * um device falando o protocolo, e duplicar essa lógica em Rust pra depois
 * duplicar em Kotlin seria pagar três vezes pela mesma coisa.
 */
export async function invoke<T>(cmd: string, args?: Args): Promise<T> {
  const route = SERVER_ROUTES[cmd];
  if (route) return route(args) as Promise<T>;
  if (isTauri()) return tauriInvoke<T>(cmd, args as any);
  throw new BrowserUnsupported(cmd);
}

/** True quando o comando funciona no contexto atual. */
export function supports(cmd: string): boolean {
  return cmd in SERVER_ROUTES || isTauri();
}
