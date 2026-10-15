import { createStore } from "./store";

/** Which global overlay is open. */
export const overlay = createStore<null | "palette" | "shortcuts">(null);

export const openPalette = () => overlay.set("palette");
export const openShortcuts = () => overlay.set("shortcuts");
export const closeOverlay = () => overlay.set(null);

/** Opens the browser file picker; resolves with the chosen files. */
export function pickFiles(accept?: string, multiple = true): Promise<File[]> {
  return new Promise((resolve) => {
    const input = document.createElement("input");
    input.type = "file";
    input.multiple = multiple;
    if (accept) input.accept = accept;
    input.onchange = () => resolve([...(input.files ?? [])]);
    input.click();
  });
}
