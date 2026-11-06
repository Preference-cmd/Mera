import { create } from "zustand";

/**
 * Persisted shell preferences (localStorage). Language belongs to paraglide
 * and the theme to `ThemeProvider`; this store holds what neither owns.
 */

export const SETTINGS_STORAGE_KEY = "reimagine.settings";

/** The persisted shape. Only keys listed here are read back or written. */
export type StoredSettings = {
  sidebarCollapsed: boolean;
};

type SettingsState = StoredSettings & {
  setSidebarCollapsed: (v: boolean) => void;
};

export const SETTINGS_DEFAULTS: StoredSettings = {
  sidebarCollapsed: false,
};

/**
 * Parse a stored blob into known settings. Unknown keys (including fields an
 * older build stored) and values of the wrong type are ignored, so a stale or
 * corrupt blob falls back to defaults instead of failing.
 */
export function parseStoredSettings(raw: string | null): StoredSettings {
  const settings = { ...SETTINGS_DEFAULTS };
  if (!raw) return settings;
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return settings;
  }
  if (typeof parsed !== "object" || parsed === null) return settings;
  const record = parsed as Record<string, unknown>;
  if (typeof record.sidebarCollapsed === "boolean") {
    settings.sidebarCollapsed = record.sidebarCollapsed;
  }
  return settings;
}

function loadSettings(): StoredSettings {
  try {
    return parseStoredSettings(localStorage.getItem(SETTINGS_STORAGE_KEY));
  } catch {
    return { ...SETTINGS_DEFAULTS };
  }
}

/** Writes only the known keys, which drops anything an older build stored. */
function persistSettings(state: StoredSettings) {
  const stored: StoredSettings = { sidebarCollapsed: state.sidebarCollapsed };
  try {
    localStorage.setItem(SETTINGS_STORAGE_KEY, JSON.stringify(stored));
  } catch {
    // Storage can be full or disabled; the in-memory value still applies.
  }
}

export const useSettingsStore = create<SettingsState>()((set, get) => ({
  ...loadSettings(),

  setSidebarCollapsed: (v) => {
    set({ sidebarCollapsed: v });
    persistSettings(get());
  },
}));
