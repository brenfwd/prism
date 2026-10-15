// Keyboard mapping for the VNC console.
//
// Keys are sent as XT (set 1) scancodes via QEMU's extended key event, so
// the guest's own keyboard layout applies. `0xE0`-prefixed scancodes are
// written as 0xE0xx. Keysyms are sent alongside for servers without the
// extension.

/** `KeyboardEvent.code` → XT scancode. */
const SCANCODES: Record<string, number> = {
  Escape: 0x01, Digit1: 0x02, Digit2: 0x03, Digit3: 0x04, Digit4: 0x05, Digit5: 0x06, Digit6: 0x07,
  Digit7: 0x08, Digit8: 0x09, Digit9: 0x0a, Digit0: 0x0b, Minus: 0x0c, Equal: 0x0d, Backspace: 0x0e,
  Tab: 0x0f, KeyQ: 0x10, KeyW: 0x11, KeyE: 0x12, KeyR: 0x13, KeyT: 0x14, KeyY: 0x15, KeyU: 0x16,
  KeyI: 0x17, KeyO: 0x18, KeyP: 0x19, BracketLeft: 0x1a, BracketRight: 0x1b, Enter: 0x1c,
  ControlLeft: 0x1d, KeyA: 0x1e, KeyS: 0x1f, KeyD: 0x20, KeyF: 0x21, KeyG: 0x22, KeyH: 0x23,
  KeyJ: 0x24, KeyK: 0x25, KeyL: 0x26, Semicolon: 0x27, Quote: 0x28, Backquote: 0x29,
  ShiftLeft: 0x2a, Backslash: 0x2b, KeyZ: 0x2c, KeyX: 0x2d, KeyC: 0x2e, KeyV: 0x2f, KeyB: 0x30,
  KeyN: 0x31, KeyM: 0x32, Comma: 0x33, Period: 0x34, Slash: 0x35, ShiftRight: 0x36,
  NumpadMultiply: 0x37, AltLeft: 0x38, Space: 0x39, CapsLock: 0x3a, F1: 0x3b, F2: 0x3c, F3: 0x3d,
  F4: 0x3e, F5: 0x3f, F6: 0x40, F7: 0x41, F8: 0x42, F9: 0x43, F10: 0x44, NumLock: 0x45,
  ScrollLock: 0x46, Numpad7: 0x47, Numpad8: 0x48, Numpad9: 0x49, NumpadSubtract: 0x4a,
  Numpad4: 0x4b, Numpad5: 0x4c, Numpad6: 0x4d, NumpadAdd: 0x4e, Numpad1: 0x4f, Numpad2: 0x50,
  Numpad3: 0x51, Numpad0: 0x52, NumpadDecimal: 0x53, IntlBackslash: 0x56, F11: 0x57, F12: 0x58,
  NumpadEqual: 0x59, F13: 0x64, F14: 0x65, F15: 0x66, F16: 0x67, F17: 0x68, F18: 0x69, F19: 0x6a,
  F20: 0x6b, F21: 0x6c, F22: 0x6d, F23: 0x6e, KanaMode: 0x70, Lang2: 0x71, Lang1: 0x72,
  IntlRo: 0x73, F24: 0x76, Convert: 0x79, NonConvert: 0x7b, IntlYen: 0x7d, NumpadComma: 0x7e,
  MediaTrackPrevious: 0xe010, MediaTrackNext: 0xe019, NumpadEnter: 0xe01c, ControlRight: 0xe01d,
  AudioVolumeMute: 0xe020, LaunchApp2: 0xe021, MediaPlayPause: 0xe022, MediaStop: 0xe024,
  AudioVolumeDown: 0xe02e, AudioVolumeUp: 0xe030, BrowserHome: 0xe032, NumpadDivide: 0xe035,
  PrintScreen: 0xe037, AltRight: 0xe038, Pause: 0xe046, Home: 0xe047, ArrowUp: 0xe048,
  PageUp: 0xe049, ArrowLeft: 0xe04b, ArrowRight: 0xe04d, End: 0xe04f, ArrowDown: 0xe050,
  PageDown: 0xe051, Insert: 0xe052, Delete: 0xe053, MetaLeft: 0xe05b, MetaRight: 0xe05c,
  ContextMenu: 0xe05d, Power: 0xe05e, Sleep: 0xe05f, WakeUp: 0xe063, BrowserSearch: 0xe065,
  BrowserFavorites: 0xe066, BrowserRefresh: 0xe067, BrowserStop: 0xe068, BrowserForward: 0xe069,
  BrowserBack: 0xe06a, LaunchApp1: 0xe06b, LaunchMail: 0xe06c, MediaSelect: 0xe06d,
};

/** X11 keysyms for non-printing `KeyboardEvent.key` values. */
const NAMED_KEYSYMS: Record<string, number> = {
  Backspace: 0xff08, Tab: 0xff09, Enter: 0xff0d, Pause: 0xff13, ScrollLock: 0xff14, Escape: 0xff1b,
  Home: 0xff50, ArrowLeft: 0xff51, ArrowUp: 0xff52, ArrowRight: 0xff53, ArrowDown: 0xff54,
  PageUp: 0xff55, PageDown: 0xff56, End: 0xff57, PrintScreen: 0xff61, Insert: 0xff63,
  ContextMenu: 0xff67, NumLock: 0xff7f, CapsLock: 0xffe5, Delete: 0xffff,
  F1: 0xffbe, F2: 0xffbf, F3: 0xffc0, F4: 0xffc1, F5: 0xffc2, F6: 0xffc3, F7: 0xffc4, F8: 0xffc5,
  F9: 0xffc6, F10: 0xffc7, F11: 0xffc8, F12: 0xffc9,
};

/** Keysyms for modifiers, by `code` (left/right differ). */
const MODIFIER_KEYSYMS: Record<string, number> = {
  ShiftLeft: 0xffe1, ShiftRight: 0xffe2, ControlLeft: 0xffe3, ControlRight: 0xffe4,
  AltLeft: 0xffe9, AltRight: 0xffea, MetaLeft: 0xffeb, MetaRight: 0xffec, NumpadEnter: 0xff8d,
};

export function scancodeFor(code: string): number {
  return SCANCODES[code] ?? 0;
}

/** X11 keysym for a Unicode character. */
export function charKeysym(ch: string): number {
  const cp = ch.codePointAt(0) ?? 0;
  if ((cp >= 0x20 && cp <= 0x7e) || (cp >= 0xa0 && cp <= 0xff)) return cp;
  return 0x01000000 | cp;
}

export function keysymFor(e: Pick<KeyboardEvent, "key" | "code">): number {
  const m = MODIFIER_KEYSYMS[e.code];
  if (m) return m;
  const named = NAMED_KEYSYMS[e.key];
  if (named) return named;
  if ([...e.key].length === 1) return charKeysym(e.key);
  return 0;
}

export interface KeyStroke {
  scancode: number;
  keysym: number;
  shift: boolean;
}

// US layout: characters typed by a key, unshifted and shifted.
const US_KEYS: [string, string, string][] = [
  ["Backquote", "`", "~"], ["Digit1", "1", "!"], ["Digit2", "2", "@"], ["Digit3", "3", "#"],
  ["Digit4", "4", "$"], ["Digit5", "5", "%"], ["Digit6", "6", "^"], ["Digit7", "7", "&"],
  ["Digit8", "8", "*"], ["Digit9", "9", "("], ["Digit0", "0", ")"], ["Minus", "-", "_"],
  ["Equal", "=", "+"], ["BracketLeft", "[", "{"], ["BracketRight", "]", "}"], ["Backslash", "\\", "|"],
  ["Semicolon", ";", ":"], ["Quote", "'", "\""], ["Comma", ",", "<"], ["Period", ".", ">"],
  ["Slash", "/", "?"], ["Space", " ", " "],
  ..."abcdefghijklmnopqrstuvwxyz".split("").map((c): [string, string, string] => [`Key${c.toUpperCase()}`, c, c.toUpperCase()]),
];

const CHAR_STROKES = new Map<string, KeyStroke>();
for (const [code, plain, shifted] of US_KEYS) {
  CHAR_STROKES.set(shifted, { scancode: SCANCODES[code], keysym: charKeysym(shifted), shift: true });
  CHAR_STROKES.set(plain, { scancode: SCANCODES[code], keysym: charKeysym(plain), shift: false });
}
CHAR_STROKES.set("\n", { scancode: SCANCODES.Enter, keysym: 0xff0d, shift: false });
CHAR_STROKES.set("\t", { scancode: SCANCODES.Tab, keysym: 0xff09, shift: false });

/**
 * The keystroke that types `ch`, assuming a US layout in the guest.
 * Characters off that layout are sent by keysym alone.
 */
export function strokeFor(ch: string): KeyStroke | null {
  if (ch === "\r") return null;
  return CHAR_STROKES.get(ch) ?? { scancode: 0, keysym: charKeysym(ch), shift: false };
}

export const SHIFT = { scancode: SCANCODES.ShiftLeft, keysym: 0xffe1 };
export const CTRL_ALT_DEL = [
  { scancode: SCANCODES.ControlLeft, keysym: 0xffe3 },
  { scancode: SCANCODES.AltLeft, keysym: 0xffe9 },
  { scancode: SCANCODES.Delete, keysym: 0xffff },
];
