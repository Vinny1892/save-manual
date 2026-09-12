/**
 * Gamepad input service. Polls `navigator.getGamepads()` via rAF, maps the
 * standard layout (Xbox/PS/Switch Pro all normalize to this in WebView2)
 * into semantic events, and runs a handler the caller registers.
 *
 * Why polling instead of events: the only DOM event is `gamepadconnected`
 * — there's no per-button event. Polling at vsync is the canonical pattern.
 * Cost is one navigator call + ~30 button compares per frame, negligible.
 *
 * Stick handling uses a deadzone + hysteresis (state stays "on" until the
 * axis crosses back below the deadzone) so drift from worn controllers
 * doesn't fire spurious direction events.
 *
 * Auto-repeat is enabled for directional events (DPad + sticks) so holding
 * down/right scrolls through a list. Threshold 400ms, then every 100ms.
 */
import { writable } from "svelte/store";
import { browser } from "$app/environment";

export type GamepadSemanticEvent =
  | "select"
  | "back"
  | "start"
  | "up"
  | "down"
  | "left"
  | "right"
  | "pageUp"
  | "pageDown";

/** Public store: true while at least one gamepad is connected & visible. */
export const gamepadConnected = writable(false);

// Standard gamepad layout — matches Xbox One/Series, PS4/5 DualSense,
// 8BitDo Pro 2, Switch Pro (via XInput driver). See:
// https://w3c.github.io/gamepad/#dfn-standard-gamepad
const BTN = {
  A: 0, // South face — select/confirm
  B: 1, // East face — back
  X: 2,
  Y: 3,
  L1: 4,
  R1: 5,
  L2: 6,
  R2: 7,
  SELECT: 8,
  START: 9,
  L3: 10,
  R3: 11,
  UP: 12,
  DOWN: 13,
  LEFT: 14,
  RIGHT: 15,
} as const;

// Stick deadzone — below this the axis is treated as centered. 0.5 is
// generous; drift from worn controllers typically maxes around 0.2-0.3.
const DEADZONE = 0.5;
// Time to hold before auto-repeat kicks in.
const REPEAT_INITIAL_MS = 400;
// Interval between repeats once it kicks in.
const REPEAT_TICK_MS = 100;

interface ButtonState {
  prev: boolean;
  pressedAt: number;
  lastRepeatAt: number;
}

interface StickDirState {
  prev: boolean;
  pressedAt: number;
  lastRepeatAt: number;
}

const buttons = new Map<number, ButtonState>();
const stickDirs: Record<"up" | "down" | "left" | "right", StickDirState> = {
  up: { prev: false, pressedAt: 0, lastRepeatAt: 0 },
  down: { prev: false, pressedAt: 0, lastRepeatAt: 0 },
  left: { prev: false, pressedAt: 0, lastRepeatAt: 0 },
  right: { prev: false, pressedAt: 0, lastRepeatAt: 0 },
};

type Handler = (ev: GamepadSemanticEvent) => void;
let handler: Handler | null = null;

export function setGamepadHandler(h: Handler | null) {
  handler = h;
}

function emit(ev: GamepadSemanticEvent) {
  handler?.(ev);
}

/**
 * Track a button: emit on press transition, optionally auto-repeat while held.
 * Updates the cached state map.
 */
function pollButton(
  pad: Gamepad,
  idx: number,
  ev: GamepadSemanticEvent,
  repeatable: boolean,
  now: number,
) {
  const pressed = pad.buttons[idx]?.pressed ?? false;
  const state = buttons.get(idx) ?? {
    prev: false,
    pressedAt: 0,
    lastRepeatAt: 0,
  };
  if (pressed && !state.prev) {
    emit(ev);
    state.pressedAt = now;
    state.lastRepeatAt = now;
  } else if (pressed && state.prev && repeatable) {
    const heldFor = now - state.pressedAt;
    if (
      heldFor > REPEAT_INITIAL_MS &&
      now - state.lastRepeatAt > REPEAT_TICK_MS
    ) {
      emit(ev);
      state.lastRepeatAt = now;
    }
  }
  state.prev = pressed;
  buttons.set(idx, state);
}

/** Treat a deadzone-thresholded axis as a directional "button". */
function pollStickDir(
  name: "up" | "down" | "left" | "right",
  active: boolean,
  now: number,
) {
  const state = stickDirs[name];
  if (active && !state.prev) {
    emit(name);
    state.pressedAt = now;
    state.lastRepeatAt = now;
  } else if (active && state.prev) {
    const heldFor = now - state.pressedAt;
    if (
      heldFor > REPEAT_INITIAL_MS &&
      now - state.lastRepeatAt > REPEAT_TICK_MS
    ) {
      emit(name);
      state.lastRepeatAt = now;
    }
  }
  state.prev = active;
}

let raf = 0;

function poll(now: number) {
  const pads = navigator.getGamepads?.() ?? [];
  let pad: Gamepad | null = null;
  for (const p of pads) {
    if (p && p.connected) {
      pad = p;
      break;
    }
  }
  if (!pad) {
    gamepadConnected.set(false);
    raf = requestAnimationFrame(poll);
    return;
  }
  gamepadConnected.set(true);

  pollButton(pad, BTN.A, "select", false, now);
  pollButton(pad, BTN.B, "back", false, now);
  pollButton(pad, BTN.START, "start", false, now);
  pollButton(pad, BTN.L1, "pageUp", true, now);
  pollButton(pad, BTN.R1, "pageDown", true, now);
  pollButton(pad, BTN.UP, "up", true, now);
  pollButton(pad, BTN.DOWN, "down", true, now);
  pollButton(pad, BTN.LEFT, "left", true, now);
  pollButton(pad, BTN.RIGHT, "right", true, now);

  // Left stick → direction events. Axes: 0=LX, 1=LY (down is positive).
  const lx = pad.axes[0] ?? 0;
  const ly = pad.axes[1] ?? 0;
  pollStickDir("up", ly < -DEADZONE, now);
  pollStickDir("down", ly > DEADZONE, now);
  pollStickDir("left", lx < -DEADZONE, now);
  pollStickDir("right", lx > DEADZONE, now);

  raf = requestAnimationFrame(poll);
}

function onConnect() {
  gamepadConnected.set(true);
}
function onDisconnect() {
  // Other pads may still be active; the next poll cycle settles the store.
  gamepadConnected.set(false);
}

/**
 * Start polling. Returns a cleanup function the caller should invoke on
 * teardown (e.g. layout onMount). Safe to call multiple times — only the
 * latest cleanup is honored, but the rAF chain isn't duplicated.
 */
export function startGamepadService(): () => void {
  if (!browser) return () => {};
  window.addEventListener("gamepadconnected", onConnect);
  window.addEventListener("gamepaddisconnected", onDisconnect);
  cancelAnimationFrame(raf);
  raf = requestAnimationFrame(poll);
  return () => {
    cancelAnimationFrame(raf);
    window.removeEventListener("gamepadconnected", onConnect);
    window.removeEventListener("gamepaddisconnected", onDisconnect);
    buttons.clear();
    for (const dir of Object.values(stickDirs)) {
      dir.prev = false;
      dir.pressedAt = 0;
      dir.lastRepeatAt = 0;
    }
    gamepadConnected.set(false);
  };
}
