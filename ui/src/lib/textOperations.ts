import * as m from "$paraglide/messages";

export const PINNED_TEXT_TOOLS_STORAGE_KEY = "reimagine.ui.pinnedTextTools";
export const DEFAULT_PINNED_TEXT_TOOLS = ["text.replace"] as const;

export function readPinnedTextTools(): string[] {
  try {
    const raw = window.localStorage.getItem(PINNED_TEXT_TOOLS_STORAGE_KEY);
    if (!raw) return [...DEFAULT_PINNED_TEXT_TOOLS];
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [...DEFAULT_PINNED_TEXT_TOOLS];
    const valid = parsed.filter(
      (item): item is string => typeof item === "string" && item.length > 0,
    );
    return valid;
  } catch {
    return [...DEFAULT_PINNED_TEXT_TOOLS];
  }
}

export function writePinnedTextTools(tools: string[]): void {
  try {
    window.localStorage.setItem(PINNED_TEXT_TOOLS_STORAGE_KEY, JSON.stringify(tools));
  } catch {
    // Ignore storage quota or disabled storage errors.
  }
}

export function textOperationLabel(opName: string): string {
  switch (opName) {
    case "text.replace":
      return m["collections.textTool.opLabel.text.replace"]();
    case "text.normalize":
      return m["collections.textTool.opLabel.text.normalize"]();
    case "text.template":
      return m["collections.textTool.opLabel.text.template"]();
    case "text.inspect":
      return m["collections.textTool.opLabel.text.inspect"]();
    default:
      return opName;
  }
}
