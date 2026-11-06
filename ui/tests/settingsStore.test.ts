import { beforeEach, describe, expect, test, vi } from "vitest";
import { parseStoredSettings, SETTINGS_STORAGE_KEY } from "../src/store/settingsStore";

const LEGACY_BLOB = JSON.stringify({
  autoSave: true,
  restoreSession: false,
  checkUpdates: true,
  gridStyle: "lines",
  minimap: false,
  cmdPaletteKey: "⌘K",
  sidebarCollapsed: true,
  backend: "Burn",
  device: "gpu0",
  memoryBudget: "8GB",
  projectDir: "~/Reimagine",
  autosaveInterval: "30s",
  downloadDir: "~/.cache/reimagine/models",
  autoConvert: false,
});

describe("parseStoredSettings", () => {
  test("keeps known keys from a legacy blob and ignores removed ones", () => {
    expect(parseStoredSettings(LEGACY_BLOB)).toEqual({ sidebarCollapsed: true });
  });

  test("falls back to defaults for missing, corrupt or mistyped blobs", () => {
    expect(parseStoredSettings(null)).toEqual({ sidebarCollapsed: false });
    expect(parseStoredSettings("{not json")).toEqual({ sidebarCollapsed: false });
    expect(parseStoredSettings("[1,2]")).toEqual({ sidebarCollapsed: false });
    expect(parseStoredSettings('{"sidebarCollapsed":"yes"}')).toEqual({
      sidebarCollapsed: false,
    });
  });
});

describe("useSettingsStore with a legacy blob", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.resetModules();
  });

  test("loads it and drops the removed keys on the next write", async () => {
    localStorage.setItem(SETTINGS_STORAGE_KEY, LEGACY_BLOB);
    const { useSettingsStore } = await import("../src/store/settingsStore");

    const state = useSettingsStore.getState();
    expect(state.sidebarCollapsed).toBe(true);
    expect(state).not.toHaveProperty("autoSave");

    state.setSidebarCollapsed(false);
    expect(JSON.parse(localStorage.getItem(SETTINGS_STORAGE_KEY) ?? "{}")).toEqual({
      sidebarCollapsed: false,
    });
  });
});
