import { createStore, local } from "./store";

export type ThemePref = "system" | "light" | "dark";

const KEY = "prism.theme";
const media = window.matchMedia("(prefers-color-scheme: dark)");

function read(): ThemePref {
  const v = local.get(KEY);
  return v === "light" || v === "dark" ? v : "system";
}

export const theme = createStore<ThemePref>(read());
/** The resolved theme. */
export const dark = createStore(false);

function apply() {
  const pref = theme.get();
  const isDark = pref === "dark" || (pref === "system" && media.matches);
  document.documentElement.classList.toggle("dark", isDark);
  document.documentElement.style.colorScheme = isDark ? "dark" : "light";
  dark.set(isDark);
}

theme.subscribe(() => {
  local.set(KEY, theme.get() === "system" ? null : theme.get());
  apply();
});
media.addEventListener("change", apply);
apply();

/** Flips between light and dark (leaving "follow the system"). */
export function toggleTheme() {
  const next = dark.get() ? "light" : "dark";
  theme.set(next === (media.matches ? "dark" : "light") ? "system" : next);
}
