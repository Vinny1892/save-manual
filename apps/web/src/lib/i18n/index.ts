/**
 * i18n setup. 3 idiomas suportados (pt-BR / en / es). Default segue o
 * `navigator.language` na primeira execução, depois persiste em localStorage.
 *
 * Uso no template:
 *   <span>{$_("key.path")}</span>
 *   <span>{$_("key.with_param", { values: { n: 5 } })}</span>
 *
 * Pra erros estruturados vindos do backend (formato AppError), usar
 * `tErr()` deste módulo — mapeia `{code, ...params}` pra string traduzida.
 */
import { browser } from "$app/environment";
import { init, register, locale, _ } from "svelte-i18n";
import { get } from "svelte/store";

export const SUPPORTED_LOCALES = ["pt-BR", "en", "es"] as const;
export type Locale = typeof SUPPORTED_LOCALES[number];
const DEFAULT_LOCALE: Locale = "pt-BR";
const STORAGE_KEY = "save-sync-locale";

register("pt-BR", () => import("./pt-BR.json"));
register("en", () => import("./en.json"));
register("es", () => import("./es.json"));

function detectInitialLocale(): Locale {
  if (!browser) return DEFAULT_LOCALE;
  const stored = localStorage.getItem(STORAGE_KEY);
  if (stored && (SUPPORTED_LOCALES as readonly string[]).includes(stored)) {
    return stored as Locale;
  }
  // Fall back to navigator.language, normalized
  const nav = (navigator.language || "").toLowerCase();
  if (nav.startsWith("pt")) return "pt-BR";
  if (nav.startsWith("es")) return "es";
  return "en";
}

init({
  fallbackLocale: "en",
  initialLocale: detectInitialLocale(),
});

export { locale };

export function setLocale(loc: Locale) {
  locale.set(loc);
  if (browser) localStorage.setItem(STORAGE_KEY, loc);
}

/** Cycles to the next locale in `SUPPORTED_LOCALES`. */
export function nextLocale(current: string): Locale {
  const idx = SUPPORTED_LOCALES.indexOf(current as Locale);
  return SUPPORTED_LOCALES[(idx + 1) % SUPPORTED_LOCALES.length];
}

/** Pretty 2-letter label shown in the locale-toggle button. */
export function localeLabel(loc: string): string {
  switch (loc) {
    case "pt-BR": return "BR";
    case "en":    return "EN";
    case "es":    return "ES";
    default:      return loc.slice(0, 2).toUpperCase();
  }
}

/**
 * Backend error translator. AppError serializes as `{ code: "...", ...params }`.
 * Plain strings (legacy/Other) pass through with a `errors.unknown` wrapping
 * fallback so the user still sees something useful.
 */
/**
 * Coerce arbitrary error params to scalar values svelte-i18n accepts
 * (strings/numbers/booleans/dates). Anything else becomes `String(v)`.
 */
function normalizeErrParams(
  obj: Record<string, unknown>,
): Record<string, string | number | boolean> {
  const out: Record<string, string | number | boolean> = {};
  for (const [k, v] of Object.entries(obj)) {
    if (typeof v === "string" || typeof v === "number" || typeof v === "boolean") {
      out[k] = v;
    } else if (v != null) {
      out[k] = String(v);
    }
  }
  return out;
}

export function tErr(e: unknown): string {
  const t = get(_);
  if (e && typeof e === "object" && "code" in e) {
    const obj = e as { code: string; [k: string]: unknown };
    const key = `errors.${obj.code}`;
    return t(key, {
      values: normalizeErrParams(obj),
      default: String(e),
    });
  }
  if (typeof e === "string") {
    // Many backend commands still return raw strings — try as key first,
    // fall back to the string itself.
    const asKey = `errors.${e}`;
    return t(asKey, { default: e });
  }
  return String(e);
}
