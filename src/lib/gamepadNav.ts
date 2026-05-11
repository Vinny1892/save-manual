/**
 * Maps the semantic events from `gamepad.ts` to DOM navigation:
 *
 *   select  → click focused element
 *   back    → history.back() (fallback: root)
 *   up/down/left/right → spatial-nearest focusable in that direction
 *   pageUp/Down → scroll viewport
 *   start   → no-op for v1 (reserved for a future help overlay)
 *
 * Spatial nav (vs pure tab order) matters for grids — saves list lays out
 * cards in rows, so DPad-right should land on the visual neighbor, not the
 * tab-order successor. The score combines on-axis distance + 2x off-axis
 * penalty, which is the standard "preferred direction" heuristic.
 */
import type { GamepadSemanticEvent } from "./gamepad";

const FOCUSABLE_SELECTOR =
  'button:not(:disabled), [href], input:not(:disabled), select:not(:disabled), textarea:not(:disabled), [tabindex]:not([tabindex="-1"])';

function isVisible(el: Element): el is HTMLElement {
  if (!(el instanceof HTMLElement)) return false;
  // offsetParent is null when display:none or detached — cheap check.
  if (el.offsetParent === null && el !== document.body) return false;
  const rect = el.getBoundingClientRect();
  return rect.width > 0 && rect.height > 0;
}

function getFocusables(): HTMLElement[] {
  return Array.from(
    document.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR),
  ).filter(isVisible);
}

function bringIntoView(el: HTMLElement) {
  el.scrollIntoView({ behavior: "smooth", block: "nearest", inline: "nearest" });
}

/**
 * Picks the focusable visually closest in `dir` from the active element.
 * Falls back to the first focusable if nothing is focused yet.
 */
function focusInDirection(dir: "up" | "down" | "left" | "right") {
  const list = getFocusables();
  if (list.length === 0) return;
  const active = document.activeElement as HTMLElement | null;
  if (!active || !list.includes(active)) {
    list[0].focus();
    bringIntoView(list[0]);
    return;
  }
  const r = active.getBoundingClientRect();
  const ax = r.left + r.width / 2;
  const ay = r.top + r.height / 2;

  let best: HTMLElement | null = null;
  let bestScore = Infinity;

  for (const cand of list) {
    if (cand === active) continue;
    const cr = cand.getBoundingClientRect();
    const cx = cr.left + cr.width / 2;
    const cy = cr.top + cr.height / 2;
    const dx = cx - ax;
    const dy = cy - ay;

    // Direction filter — must be strictly past the active element on the
    // primary axis. 1px tolerance lets us catch flush neighbors.
    if (dir === "up" && dy >= -1) continue;
    if (dir === "down" && dy <= 1) continue;
    if (dir === "left" && dx >= -1) continue;
    if (dir === "right" && dx <= 1) continue;

    const isVertical = dir === "up" || dir === "down";
    const onAxis = Math.abs(isVertical ? dy : dx);
    const offAxis = Math.abs(isVertical ? dx : dy);
    // 2x off-axis penalty keeps the cursor on the "same row/column" feel.
    const score = onAxis + offAxis * 2;

    if (score < bestScore) {
      bestScore = score;
      best = cand;
    }
  }

  if (best) {
    best.focus();
    bringIntoView(best);
  }
}

/** Click whatever is focused, falling back to nothing if not a clickable. */
function activateFocused() {
  const el = document.activeElement;
  if (el instanceof HTMLElement && typeof el.click === "function") {
    el.click();
  }
}

function goBack() {
  if (window.history.length > 1) {
    window.history.back();
  } else {
    // Fresh tab fallback — return to root.
    window.location.assign("/");
  }
}

export function handleGamepadEvent(ev: GamepadSemanticEvent) {
  switch (ev) {
    case "select":
      activateFocused();
      break;
    case "back":
      goBack();
      break;
    case "up":
    case "down":
    case "left":
    case "right":
      focusInDirection(ev);
      break;
    case "pageUp":
      window.scrollBy({ top: -window.innerHeight * 0.6, behavior: "smooth" });
      break;
    case "pageDown":
      window.scrollBy({ top: window.innerHeight * 0.6, behavior: "smooth" });
      break;
    case "start":
      // Reserved.
      break;
  }
}
