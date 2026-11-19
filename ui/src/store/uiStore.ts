import { create } from "zustand";
import type { Viewport } from "@xyflow/react";
import type { ActivityActor } from "@/ipc/schemas";

/**
 * UI shell state — session panel, canvas focus, command palette,
 * and the settings dialog.
 * View-only state — deliberately kept out of the board (undo) store.
 * Route navigation is handled by TanStack Router (URL-based).
 */

export type PaletteMode = { kind: "closed" } | { kind: "command" };

/** Sections of the settings dialog; `workspace-*` belong to the active workspace. */
export type SettingsSection =
  | "general"
  | "appearance"
  | "shortcuts"
  | "storage"
  | "about"
  | "workspace-general"
  | "workspace-folders";

/**
 * Canvas highlight requested by hovering an activity record. Items whose id is
 * in `targets` keep full ink plus a ring; every other item fades. `actorKind`
 * chooses the ring colour: agent gold for Agent, accent for everyone else.
 */
export type CanvasTrace = {
  targets: string[];
  actorKind: ActivityActor["kind"];
};

type UIState = {
  // ── Session panel ───────────────────────────────────────
  sessionPanelOpen: boolean;
  setSessionPanelOpen: (open: boolean) => void;
  toggleSessionPanel: () => void;

  // ── Canvas focus ────────────────────────────────────────
  /** Item the canvas should center on its next render; cleared once handled. */
  canvasFocusItemId: string | null;
  requestCanvasFocus: (id: string) => void;
  clearCanvasFocus: () => void;

  // ── Canvas viewports ────────────────────────────────────
  /**
   * React Flow viewport per project, persisted by the canvas on every pan or
   * zoom end. The canvas restores it on mount and on project switches so a
   * canvas → library → canvas trip keeps the view, and the library derives the
   * placement center from it.
   */
  canvasViewports: Record<string, Viewport>;
  setCanvasViewport: (projectId: string, viewport: Viewport) => void;

  // ── Canvas trace ────────────────────────────────────────
  /** Hover trace from the activity panel; null when nothing is hovered. */
  canvasTrace: CanvasTrace | null;
  setCanvasTrace: (trace: CanvasTrace) => void;
  clearCanvasTrace: () => void;

  // ── Settings dialog ─────────────────────────────────────
  /**
   * The settings dialog floats over the current route, so opening it never
   * changes the URL, the board viewport or the selection.
   */
  settingsOpen: boolean;
  settingsSection: SettingsSection;
  /**
   * Workspace the workspace group edits while the dialog is open; null means
   * the active workspace. Cleared on close.
   */
  settingsWorkspaceId: string | null;
  /**
   * Open the dialog on `section` when given, otherwise where it was last left.
   * `workspaceId` targets another workspace's group without switching to it.
   */
  openSettings: (section?: SettingsSection, workspaceId?: string) => void;
  closeSettings: () => void;
  setSettingsSection: (section: SettingsSection) => void;

  // ── Command palette ─────────────────────────────────────
  palette: PaletteMode;
  openCommandPalette: () => void;
  closePalette: () => void;

  assetLibraryViewMode: "grid" | "list";
  setAssetLibraryViewMode: (mode: "grid" | "list") => void;

  canvasLibraryPanelOpen: boolean;
  setCanvasLibraryPanelOpen: (open: boolean) => void;
  toggleCanvasLibraryPanel: () => void;
};

const SESSION_PANEL_STORAGE_KEY = "mera.sessionPanelOpen";

function loadSessionPanelOpen(): boolean {
  try {
    const raw = localStorage.getItem(SESSION_PANEL_STORAGE_KEY);
    if (raw === "true") return true;
    if (raw === "false") return false;
  } catch {
    // Ignore storage read errors and fall back to default.
  }
  return true;
}

function persistSessionPanelOpen(open: boolean): void {
  try {
    localStorage.setItem(SESSION_PANEL_STORAGE_KEY, String(open));
  } catch {
    // Ignore storage write errors.
  }
}

export const useUIStore = create<UIState>()((set) => ({
  // Session panel (D5: expanded on first open, persisted thereafter)
  sessionPanelOpen: loadSessionPanelOpen(),
  setSessionPanelOpen: (open: boolean) => {
    persistSessionPanelOpen(open);
    set({ sessionPanelOpen: open });
  },
  toggleSessionPanel: () =>
    set((state) => {
      const next = !state.sessionPanelOpen;
      persistSessionPanelOpen(next);
      return { sessionPanelOpen: next };
    }),

  // Canvas focus
  canvasFocusItemId: null,
  requestCanvasFocus: (id: string) => set({ canvasFocusItemId: id }),
  clearCanvasFocus: () => set({ canvasFocusItemId: null }),

  // Canvas viewports
  canvasViewports: {},
  setCanvasViewport: (projectId, viewport) =>
    set((state) => ({
      canvasViewports: { ...state.canvasViewports, [projectId]: viewport },
    })),

  // Canvas trace
  canvasTrace: null,
  setCanvasTrace: (trace) => set({ canvasTrace: trace }),
  clearCanvasTrace: () => set({ canvasTrace: null }),

  // Settings dialog
  settingsOpen: false,
  settingsSection: "general",
  settingsWorkspaceId: null,
  openSettings: (section, workspaceId) =>
    set((state) => ({
      settingsOpen: true,
      settingsSection: section ?? state.settingsSection,
      settingsWorkspaceId: workspaceId ?? null,
      palette: { kind: "closed" },
    })),
  closeSettings: () => set({ settingsOpen: false, settingsWorkspaceId: null }),
  setSettingsSection: (section) => set({ settingsSection: section }),

  // Command palette
  palette: { kind: "closed" },
  openCommandPalette: () =>
    set((state) => ({
      palette: state.palette.kind === "command" ? { kind: "closed" } : { kind: "command" },
    })),
  closePalette: () => set({ palette: { kind: "closed" } }),

  // Asset Library view mode
  assetLibraryViewMode: "grid",
  setAssetLibraryViewMode: (mode) => set({ assetLibraryViewMode: mode }),

  // Canvas library panel
  canvasLibraryPanelOpen: false,
  setCanvasLibraryPanelOpen: (open) => set({ canvasLibraryPanelOpen: open }),
  toggleCanvasLibraryPanel: () =>
    set((state) => ({ canvasLibraryPanelOpen: !state.canvasLibraryPanelOpen })),
}));

/**
 * Whether a window-level shortcut handler should stand down because a modal
 * surface owns the keyboard. Read at event time, outside React.
 */
export function isShellModalOpen(): boolean {
  return useUIStore.getState().settingsOpen;
}
