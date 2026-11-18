import { describe, test, expect, vi, beforeEach, afterEach } from "vitest";
import type { ReactNode } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { toast } from "sonner";
import { act, fireEvent, render, screen, waitFor } from "./test-utils";
import { CanvasEmptyState } from "../src/board/CanvasEmptyState";
import { ImageThumbnail } from "../src/components/ui/ImageThumbnail";
import { WorkspaceFolders } from "../src/components/settings/WorkspaceFolders";
import { LibraryView } from "../src/components/layout/LibraryView";
import { placeLibraryItems } from "../src/lib/globalLibrary";
import * as ipc from "../src/ipc";
import { mockGetBoardSnapshot, mockOpInvoke, mockResetBoard, mockUndoBoard } from "../src/ipc/mock";
import { useProjectStore } from "../src/store/project";
import { useUIStore } from "../src/store/uiStore";

const mockOpenDialog = vi.fn();
vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: (...args: unknown[]) => mockOpenDialog(...args),
}));

vi.mock("@tanstack/react-router", () => ({
  Link: ({ to, children, ...props }: { to: string; children: ReactNode }) => (
    <a href={to} {...props}>
      {children}
    </a>
  ),
  useNavigate: () => vi.fn(),
  useSearch: () => ({}),
}));

vi.mock("sonner", () => ({
  toast: {
    error: vi.fn(),
    success: vi.fn(),
    info: vi.fn(),
  },
}));

describe("Asset & Canvas UX Pass (U1–U4)", () => {
  let queryClient: QueryClient;

  beforeEach(() => {
    vi.clearAllMocks();
    localStorage.clear();
    mockResetBoard("proj-ux");
    useProjectStore.setState({ activeProjectId: "proj-ux" });
    useUIStore.setState({
      sessionPanelOpen: true,
      canvasLibraryPanelOpen: false,
    });
    queryClient = new QueryClient({
      defaultOptions: { queries: { retry: false, staleTime: 0 } },
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  test("U1: CanvasEmptyState opens the canvas assets drawer instead of leaving the canvas", () => {
    const onCreateNote = vi.fn();
    render(<CanvasEmptyState onCreateNote={onCreateNote} />);

    fireEvent.click(screen.getByRole("button", { name: "Add from assets" }));
    expect(useUIStore.getState().canvasLibraryPanelOpen).toBe(true);
  });

  test("U1/D4: WorkspaceFolders removes immediately without window.confirm and offers 8s undo", async () => {
    const confirmSpy = vi.spyOn(window, "confirm").mockReturnValue(false);
    const invoked: Array<{ name: string; input: unknown }> = [];
    vi.spyOn(ipc, "opInvoke").mockImplementation(async (_proj, name, input) => {
      invoked.push({ name, input });
      if (name === "project.list_folders") {
        return {
          step: {},
          output: { folders: [{ id: "fld-1", path: "/Users/demo/shots" }] },
        };
      }
      return { step: {}, output: {} };
    });

    render(
      <QueryClientProvider client={queryClient}>
        <WorkspaceFolders projectId="proj-ux" />
      </QueryClientProvider>,
    );

    await screen.findByText("/Users/demo/shots");
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Remove" }));
    });

    expect(confirmSpy).not.toHaveBeenCalled();
    expect(invoked).toContainEqual({
      name: "project.remove_folder",
      input: { id: "fld-1" },
    });

    const toastCalls = vi.mocked(toast.success).mock.calls;
    const removeToast = toastCalls.find(([msg]) => msg === "Workspace folder removed");
    expect(removeToast).toBeDefined();
    const opts = removeToast?.[1] as { duration?: number; action?: { onClick: () => void } };
    expect(opts?.duration).toBe(8000);

    await act(async () => {
      opts?.action?.onClick();
    });
    expect(invoked).toContainEqual({
      name: "project.add_folder",
      input: { path: "/Users/demo/shots" },
    });
  });

  test("U1/D5: sessionPanelOpen persists user toggles across module reloads", async () => {
    useUIStore.getState().setSessionPanelOpen(false);
    expect(localStorage.getItem("mera.sessionPanelOpen")).toBe("false");

    vi.resetModules();
    const { useUIStore: reloadedStore } = await import("../src/store/uiStore");
    expect(reloadedStore.getState().sessionPanelOpen).toBe(false);
  });

  test("U3: ImageThumbnail falls back to icon cleanly when image fails to load", () => {
    render(<ImageThumbnail src="mera-asset://localhost/library/broken" alt="broken-preview" />);
    const img = screen.getByAltText("broken-preview");
    fireEvent.error(img);
    expect(screen.queryByAltText("broken-preview")).toBeNull();
  });

  test("U4/D3: LibraryView import dialog defaults 'Also add to Assets' to checked and invokes both", async () => {
    vi.spyOn(ipc, "isTauri").mockReturnValue(true);
    mockOpenDialog.mockResolvedValue(["/tmp/clip_01.png"]);
    const invoked: string[] = [];
    vi.spyOn(ipc, "opInvoke").mockImplementation(async (_proj, name) => {
      invoked.push(name);
      if (name === "collection.list") return { step: {}, output: { collections: [] } };
      if (name === "dynamic_workflow.list") return { step: {}, output: { workflows: [] } };
      if (name === "dynamic_workflow.list_runs") return { step: {}, output: { runs: [] } };
      if (name === "asset.list") return { step: {}, output: { assets: [] } };
      if (name === "asset.import") return { step: {}, output: { assets: ["ast-new"] } };
      if (name === "library.add") {
        return {
          step: {},
          output: {
            items: [{ id: "lib-new", name: "clip_01.png", deduplicated: false }],
            version: 1,
          },
        };
      }
      return { step: {}, output: {} };
    });

    render(
      <QueryClientProvider client={queryClient}>
        <LibraryView />
      </QueryClientProvider>,
    );

    const importBtn = await screen.findByTestId("library-import");
    await act(async () => {
      fireEvent.click(importBtn);
    });

    await screen.findByTestId("library-import-dialog");
    const checkbox = screen.getByTestId("also-add-to-assets-checkbox") as HTMLInputElement;
    expect(checkbox.checked).toBe(true);

    await act(async () => {
      fireEvent.click(screen.getByTestId("library-import-confirm"));
    });

    await waitFor(() => {
      expect(invoked).toContain("asset.import");
      expect(invoked).toContain("library.add");
    });
  });

  test("U4: library.place places single asset_ref card, multi-item collection, and supports one-step undo", async () => {
    vi.spyOn(ipc, "opInvoke").mockImplementation((projectId, name, input) =>
      mockOpInvoke({ projectId, name, input }),
    );

    const single = await placeLibraryItems(
      "proj-ux",
      [
        {
          id: "lib_item_2",
          name: "prompts_master.txt",
          tags: [],
          media_type: "text/plain",
          size: 100,
          hash: "h2",
          added_at: "2026-10-02T00:00:00Z",
          removed: false,
          source: {},
        },
      ],
      { x: 500, y: 400 },
    );

    expect(single.collectionId).toBeNull();
    expect(single.assets).toHaveLength(1);
    const boardAfterSingle = await mockGetBoardSnapshot({ projectId: "proj-ux" });
    expect(boardAfterSingle.items).toHaveLength(1);
    expect(boardAfterSingle.items[0].kind).toEqual({
      type: "asset_ref",
      artifact_id: single.assets[0],
    });
    expect(boardAfterSingle.items[0].position).toEqual({ x: 380, y: 320 });

    const multi = await placeLibraryItems(
      "proj-ux",
      [
        {
          id: "lib_item_1",
          name: "design_system_preview.png",
          tags: [],
          media_type: "image/png",
          size: 200,
          hash: "h1",
          added_at: "2026-10-01T00:00:00Z",
          removed: false,
          source: {},
        },
        {
          id: "lib_item_3",
          name: "workflow_spec.json",
          tags: [],
          media_type: "application/json",
          size: 300,
          hash: "h3",
          added_at: "2026-10-03T00:00:00Z",
          removed: false,
          source: {},
        },
      ],
      { x: 600, y: 500 },
      "UI Pack",
    );

    expect(multi.collectionId).not.toBeNull();
    const boardAfterMulti = await mockGetBoardSnapshot({ projectId: "proj-ux" });
    expect(boardAfterMulti.items).toHaveLength(2);
    expect(boardAfterMulti.items[1].kind).toEqual({
      type: "collection",
      collection_id: multi.collectionId,
    });

    await mockUndoBoard({ projectId: "proj-ux", expectedEntryId: multi.historyEntryId });
    const boardAfterUndo = await mockGetBoardSnapshot({ projectId: "proj-ux" });
    expect(boardAfterUndo.items).toHaveLength(1);
  });
});
