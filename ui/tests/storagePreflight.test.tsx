import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { ReactNode } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { toast } from "sonner";
import { fireEvent, render, screen, waitFor, within } from "./test-utils";
import { AssetLibraryView } from "../src/components/layout/AssetLibraryView";
import { ModelDownloadDialog } from "../src/components/layout/ModelDownloadDialog";
import { ModsSettingsSection } from "../src/components/settings/ModsSettingsSection";
import * as ipc from "../src/ipc";
import type { LibraryItem, ModelCard } from "../src/ipc/schemas";
import {
  mockOpInvoke,
  mockStoragePreflightWrite,
  mockStorageSetBudget,
  mockStorageStatus,
  resetMockMods,
  resetMockStorage,
  setMockStoragePathSize,
} from "../src/ipc/mock";
import { useProjectStore } from "../src/store/project";
import { useUIStore } from "../src/store/uiStore";

const mockNavigate = vi.fn();
let mockSearch: { sort?: string } = {};
const mockOpenDialog = vi.fn();
let mockDragDropHandler: ((event: { payload: { type: string; paths?: string[] } }) => void) | null =
  null;

vi.mock("sonner", () => ({
  toast: { error: vi.fn(), success: vi.fn(), info: vi.fn() },
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: (...args: unknown[]) => mockOpenDialog(...args),
}));

vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({
    onDragDropEvent: vi.fn(
      async (cb: (event: { payload: { type: string; paths?: string[] } }) => void) => {
        mockDragDropHandler = cb;
        return () => {
          mockDragDropHandler = null;
        };
      },
    ),
  }),
}));

vi.mock("@tanstack/react-router", () => ({
  Link: ({ to, children, ...props }: { to: string; children: ReactNode }) => (
    <a href={to} {...props}>
      {children}
    </a>
  ),
  useNavigate: () => mockNavigate,
  useSearch: () => mockSearch,
}));

const GIB = 1024 * 1024 * 1024;
const MIB = 1024 * 1024;

function makeQueryClient() {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, gcTime: 0 },
      mutations: { retry: false },
    },
  });
}

const SAMPLE_ITEMS: LibraryItem[] = [
  {
    id: "lib-small",
    name: "small.png",
    tags: [],
    media_type: "image/png",
    size: 512,
    hash: "h-small",
    added_at: "2026-11-18T12:00:00Z",
    removed: false,
    source: {},
  },
  {
    id: "lib-huge",
    name: "huge.mp4",
    tags: [],
    media_type: "video/mp4",
    size: 50 * MIB,
    hash: "h-huge",
    added_at: "2026-11-18T10:00:00Z",
    removed: false,
    source: {},
  },
  {
    id: "lib-mid",
    name: "mid.png",
    tags: [],
    media_type: "image/png",
    size: 2 * MIB,
    hash: "h-mid",
    added_at: "2026-11-18T11:00:00Z",
    removed: false,
    source: {},
  },
];

describe("Assets sort=size, library.purge, and storage pre-write confirmation", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    localStorage.clear();
    mockSearch = {};
    mockDragDropHandler = null;
    resetMockMods();
    resetMockStorage();
    vi.spyOn(ipc, "isTauri").mockReturnValue(true);
    useProjectStore.setState({ activeProjectId: "default" });
    useUIStore.setState({ assetLibraryViewMode: "list" });
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  test("AssetLibraryView supports ?sort=size search param, sort selector, and manual library.purge", async () => {
    mockSearch = { sort: "size" };
    const opInvokeSpy = vi.spyOn(ipc, "opInvoke").mockImplementation(async (_proj, name) => {
      if (name === "library.list") {
        return { step: {}, output: { items: SAMPLE_ITEMS } };
      }
      if (name === "library.purge") {
        return {
          step: {},
          output: { purged_entries: 2, deleted_files: 2, freed_bytes: 5 * MIB, version: 4 },
        };
      }
      return { step: {}, output: {} };
    });

    const qc = makeQueryClient();
    render(
      <QueryClientProvider client={qc}>
        <AssetLibraryView />
      </QueryClientProvider>,
    );

    await waitFor(() => {
      expect(screen.getAllByTestId("asset-row")).toHaveLength(3);
    });

    const sortSelect = screen.getByTestId("assets-sort-select") as HTMLSelectElement;
    expect(sortSelect.value).toBe("size");

    // Size descending: huge.mp4 (50 MB) -> mid.png (2 MB) -> small.png (512 B)
    const rowsBySize = screen
      .getAllByTestId("asset-row")
      .map((r) => r.getAttribute("data-asset-id"));
    expect(rowsBySize).toEqual(["lib-huge", "lib-mid", "lib-small"]);

    // Switching sort selector to name updates URL and sorts alphabetically
    fireEvent.change(sortSelect, { target: { value: "name" } });
    expect(mockNavigate).toHaveBeenCalledWith({
      to: "/assets",
      search: { sort: "name" },
      replace: true,
    });
    const rowsByName = screen
      .getAllByTestId("asset-row")
      .map((r) => r.getAttribute("data-asset-id"));
    expect(rowsByName).toEqual(["lib-huge", "lib-mid", "lib-small"]); // huge, mid, small alphabetical too

    // Switching to recent sorts newest first
    fireEvent.change(sortSelect, { target: { value: "recent" } });
    expect(mockNavigate).toHaveBeenCalledWith({
      to: "/assets",
      search: {},
      replace: true,
    });
    const rowsByRecent = screen
      .getAllByTestId("asset-row")
      .map((r) => r.getAttribute("data-asset-id"));
    expect(rowsByRecent).toEqual(["lib-small", "lib-mid", "lib-huge"]);

    // Manual Purge removed via header More menu
    const moreBtn = screen.getByTestId("assets-more-button");
    fireEvent.pointerDown(moreBtn);
    fireEvent.click(moreBtn);
    const purgeItem = await screen.findByTestId("assets-purge-removed-item");
    fireEvent.click(purgeItem);

    const purgeDialog = await screen.findByTestId("assets-purge-dialog");
    fireEvent.click(within(purgeDialog).getByTestId("assets-purge-confirm-btn"));

    await waitFor(() => {
      expect(opInvokeSpy).toHaveBeenCalledWith("default", "library.purge", {});
      expect(toast.success).toHaveBeenCalledWith(expect.stringContaining("5 MB"));
    });
  });

  test("mock library.purge removes soft-deleted items and zeroes libraryRemovedBytes", async () => {
    await mockOpInvoke({
      projectId: "default",
      name: "library.add",
      input: { paths: ["/tmp/purge-target.png"] },
    });
    const listBefore = await mockOpInvoke({
      projectId: "default",
      name: "library.list",
      input: { include_removed: true },
    });
    const added = (listBefore.output as { items: LibraryItem[] }).items.find(
      (i) => i.name === "purge-target.png",
    )!;
    await mockOpInvoke({
      projectId: "default",
      name: "library.remove",
      input: { ids: [added.id] },
    });
    const statusBefore = await mockStorageStatus();
    expect(statusBefore.categories.libraryRemovedBytes).toBeGreaterThanOrEqual(1024);

    const purgeRes = await mockOpInvoke({
      projectId: "default",
      name: "library.purge",
      input: {},
    });
    expect((purgeRes.output as { purged_entries: number }).purged_entries).toBeGreaterThanOrEqual(
      1,
    );

    const statusAfter = await mockStorageStatus();
    expect(statusAfter.categories.libraryRemovedBytes).toBe(0);
  });

  test("mockStoragePreflightWrite never interrupts small (<10 MiB) library writes even over budget", async () => {
    await mockStorageSetBudget({ limitBytes: 1 * GIB }); // mock models > 1 GiB => overBudget = true
    const smallRes = await mockStoragePreflightWrite({
      kind: "library",
      estimatedBytes: 2 * MIB,
    });
    expect(smallRes.requiresConfirm).toBe(false);
    expect(smallRes.reason).toBeNull();

    const largeRes = await mockStoragePreflightWrite({
      kind: "library",
      estimatedBytes: 25 * MIB,
    });
    expect(largeRes.requiresConfirm).toBe(true);
    expect(largeRes.reason).toBe("over_budget");

    // When estimatedBytes is 0 and paths are supplied, mock sums path sizes
    setMockStoragePathSize("/tmp/small.png", 1 * MIB);
    setMockStoragePathSize("/tmp/large.mp4", 20 * MIB);
    const smallPathRes = await mockStoragePreflightWrite({
      kind: "library",
      estimatedBytes: 0,
      paths: ["/tmp/small.png"],
    });
    expect(smallPathRes.requiresConfirm).toBe(false);

    const largePathRes = await mockStoragePreflightWrite({
      kind: "library",
      estimatedBytes: 0,
      paths: ["/tmp/large.mp4"],
    });
    expect(largePathRes.requiresConfirm).toBe(true);
    expect(largePathRes.reason).toBe("over_budget");
  });

  test("AssetLibraryView addFiles does not interrupt small adds and confirms/cancels when preflight requiresConfirm", async () => {
    const opInvokeSpy = vi.spyOn(ipc, "opInvoke").mockImplementation(async (_proj, name) => {
      if (name === "library.list") return { step: {}, output: { items: [] } };
      if (name === "library.add") {
        return {
          step: {},
          output: { items: [{ id: "lib-1", name: "photo.png", deduplicated: false }], version: 2 },
        };
      }
      return { step: {}, output: {} };
    });

    mockOpenDialog.mockResolvedValue(["/Users/demo/photo.png"]);

    // 1. Small add (requiresConfirm: false) proceeds immediately with no dialog
    const preflightSpy = vi.spyOn(ipc, "storagePreflightWrite").mockResolvedValueOnce({
      requiresConfirm: false,
      reason: null,
      projectedTotalBytes: 100 * MIB,
      limitBytes: 500 * MIB,
      availableBytes: 100 * GIB,
    });

    const qc = makeQueryClient();
    render(
      <QueryClientProvider client={qc}>
        <AssetLibraryView />
      </QueryClientProvider>,
    );

    fireEvent.click(screen.getByTestId("add-files-button"));
    await waitFor(() => {
      expect(preflightSpy).toHaveBeenCalledWith({
        kind: "library",
        estimatedBytes: 0,
        paths: ["/Users/demo/photo.png"],
      });
      expect(opInvokeSpy).toHaveBeenCalledWith("default", "library.add", {
        paths: ["/Users/demo/photo.png"],
      });
    });
    expect(screen.queryByTestId("storage-preflight-dialog")).not.toBeInTheDocument();

    // Tauri drag-drop also forwards paths to storagePreflightWrite
    await waitFor(() => expect(mockDragDropHandler).not.toBeNull());
    preflightSpy.mockResolvedValueOnce({
      requiresConfirm: true,
      reason: "would_exceed_budget",
      projectedTotalBytes: 55 * GIB,
      limitBytes: 50 * GIB,
      availableBytes: 100 * GIB,
    });
    mockDragDropHandler!({
      payload: { type: "drop", paths: ["/Users/demo/dropped-large.mp4"] },
    });
    await waitFor(() => {
      expect(preflightSpy).toHaveBeenCalledWith({
        kind: "library",
        estimatedBytes: 0,
        paths: ["/Users/demo/dropped-large.mp4"],
      });
    });
    const dropDialog = await screen.findByTestId("storage-preflight-dialog");
    expect(within(dropDialog).getByText("dropped-large.mp4")).toBeInTheDocument();
    fireEvent.click(within(dropDialog).getByTestId("storage-preflight-cancel"));
    await waitFor(() => {
      expect(screen.queryByTestId("storage-preflight-dialog")).not.toBeInTheDocument();
    });

    // 2. Preflight requiresConfirm: true -> Cancel aborts library.add
    opInvokeSpy.mockClear();
    preflightSpy.mockResolvedValueOnce({
      requiresConfirm: true,
      reason: "would_exceed_budget",
      projectedTotalBytes: 55 * GIB,
      limitBytes: 50 * GIB,
      availableBytes: 100 * GIB,
    });

    fireEvent.click(screen.getByTestId("add-files-button"));
    const dialog = await screen.findByTestId("storage-preflight-dialog");
    expect(within(dialog).getByText("photo.png")).toBeInTheDocument();
    fireEvent.click(within(dialog).getByTestId("storage-preflight-cancel"));

    await waitFor(() => {
      expect(screen.queryByTestId("storage-preflight-dialog")).not.toBeInTheDocument();
    });
    expect(opInvokeSpy).not.toHaveBeenCalledWith("default", "library.add", expect.anything());

    // 3. Preflight requiresConfirm: true -> Continue proceeds with library.add
    preflightSpy.mockResolvedValueOnce({
      requiresConfirm: true,
      reason: "over_budget",
      projectedTotalBytes: 55 * GIB,
      limitBytes: 50 * GIB,
      availableBytes: 100 * GIB,
    });

    fireEvent.click(screen.getByTestId("add-files-button"));
    const dialog2 = await screen.findByTestId("storage-preflight-dialog");
    fireEvent.click(within(dialog2).getByTestId("storage-preflight-confirm"));

    await waitFor(() => {
      expect(opInvokeSpy).toHaveBeenCalledWith("default", "library.add", {
        paths: ["/Users/demo/photo.png"],
      });
    });
  });

  test("ModelDownloadDialog runs storagePreflightWrite and aborts on cancel or downloads on confirm", async () => {
    const card: ModelCard = {
      entry: {
        id: "stabilityai/sdxl-turbo",
        author: "stabilityai",
        pipelineTag: "text-to-image",
        tags: [],
        downloads: 1000,
        likes: 50,
        lastModified: "2026-01-01",
        private: false,
      },
      detectedFormat: "safetensors",
      estimatedDownloadSize: 6 * GIB,
      modelSummary: "Fast SDXL",
      fileCount: 4,
      components: ["unet"],
    };

    vi.spyOn(ipc, "getModelCard").mockResolvedValue(card);
    const downloadSpy = vi.spyOn(ipc, "downloadHuggingfaceModel").mockResolvedValue({
      effective: true,
      provider: "huggingface",
      repoId: "stabilityai/sdxl-turbo",
      revision: "main",
      targetDir: "/models/sdxl-turbo",
      files: [],
      totalBytes: 6 * GIB,
      finishedAt: "2026-11-18T00:00:00Z",
    });

    const preflightSpy = vi.spyOn(ipc, "storagePreflightWrite").mockResolvedValue({
      requiresConfirm: true,
      reason: "would_exceed_budget",
      projectedTotalBytes: 56 * GIB,
      limitBytes: 50 * GIB,
      availableBytes: 80 * GIB,
    });

    const qc = makeQueryClient();
    const onInstalled = vi.fn();
    render(
      <QueryClientProvider client={qc}>
        <ModelDownloadDialog
          open
          initialRepoId="stabilityai/sdxl-turbo"
          onClose={vi.fn()}
          onInstalled={onInstalled}
        />
      </QueryClientProvider>,
    );

    const downloadBtn = await screen.findByRole("button", { name: /^Download$/i });
    await waitFor(() => expect(downloadBtn).toBeEnabled());

    // Click Download -> shows preflight dialog -> Cancel aborts
    fireEvent.click(downloadBtn);
    await waitFor(() => {
      expect(preflightSpy).toHaveBeenCalledWith({
        kind: "model",
        estimatedBytes: 6 * GIB,
      });
    });

    const dialog = await screen.findByTestId("storage-preflight-dialog");
    expect(within(dialog).getByText("stabilityai/sdxl-turbo")).toBeInTheDocument();
    expect(within(dialog).getByText("6 GB")).toBeInTheDocument();
    fireEvent.click(within(dialog).getByTestId("storage-preflight-cancel"));

    await waitFor(() => {
      expect(screen.queryByTestId("storage-preflight-dialog")).not.toBeInTheDocument();
    });
    expect(downloadSpy).not.toHaveBeenCalled();

    // Click Download again -> Continue proceeds to downloadHuggingfaceModel
    const downloadBtnAgain = await screen.findByRole("button", { name: /^Download$/i });
    fireEvent.click(downloadBtnAgain);
    const dialog2 = await screen.findByTestId("storage-preflight-dialog");
    fireEvent.click(within(dialog2).getByTestId("storage-preflight-confirm"));

    await waitFor(() => {
      expect(downloadSpy).toHaveBeenCalledTimes(1);
    });
    await waitFor(() => {
      expect(onInstalled).toHaveBeenCalledTimes(1);
    });
  });

  test("ModsSettingsSection runs storagePreflightWrite before remote mod download and local archive install", async () => {
    vi.spyOn(ipc, "isTauri").mockReturnValue(false);
    vi.spyOn(ipc, "modsBrowse").mockResolvedValue({
      items: [
        {
          source_id: "official",
          source_kind: "official",
          source_label: "Official",
          id: "upscale_pack",
          name: "Upscale Pack",
          version: "1.2.0",
          description: "Image upscaler",
          api: "1",
          target: "universal",
          author: "Mera",
          homepage: null,
          license: "MIT",
          permissions: {
            host_paths: false,
            events_subscribe: [],
            events_publish: [],
            recommended_agent_operations: [],
            network: false,
            filesystem_note: null,
          },
          capabilities: ["image"],
          download_url: "https://example.com/upscale.tar.gz",
          sha256: "b".repeat(64),
          size_bytes: 200 * MIB,
          compatible: true,
          compatibility_reason: null,
          installed_version: null,
          update_available: false,
          source_conflict: null,
        },
      ],
      outcomes: [{ source_id: "official", source_name: "Official", ok: true, error: null }],
    });

    const downloadStartSpy = vi.spyOn(ipc, "modsDownloadStart").mockResolvedValue({
      task_id: "task-1",
      source_id: "official",
      mod_id: "upscale_pack",
      version: "1.2.0",
      status: "downloading",
      downloaded_bytes: 0,
      total_bytes: 200 * MIB,
      error: null,
      preview: null,
    });

    const preflightSpy = vi.spyOn(ipc, "storagePreflightWrite").mockResolvedValue({
      requiresConfirm: true,
      reason: "below_disk_floor",
      projectedTotalBytes: 20 * GIB,
      limitBytes: null,
      availableBytes: 15 * GIB,
    });

    const qc = makeQueryClient();
    render(
      <QueryClientProvider client={qc}>
        <ModsSettingsSection />
      </QueryClientProvider>,
    );

    // Switch to Browse tab
    fireEvent.click(screen.getByRole("tab", { name: "Browse" }));
    const modRow = await screen.findByTestId("browse-mod-upscale_pack");
    const installBtn = within(modRow).getByRole("button", { name: "Install" });

    // Cancel aborts modsDownloadStart
    fireEvent.click(installBtn);
    await waitFor(() => {
      expect(preflightSpy).toHaveBeenCalledWith({
        kind: "mod",
        estimatedBytes: 200 * MIB,
      });
    });

    const dialog = await screen.findByTestId("storage-preflight-dialog");
    expect(within(dialog).getByText("Upscale Pack")).toBeInTheDocument();
    fireEvent.click(within(dialog).getByTestId("storage-preflight-cancel"));

    await waitFor(() => {
      expect(screen.queryByTestId("storage-preflight-dialog")).not.toBeInTheDocument();
    });
    expect(downloadStartSpy).not.toHaveBeenCalled();

    // Confirm starts modsDownloadStart
    fireEvent.click(installBtn);
    const dialog2 = await screen.findByTestId("storage-preflight-dialog");
    fireEvent.click(within(dialog2).getByTestId("storage-preflight-confirm"));

    await waitFor(() => {
      expect(downloadStartSpy).toHaveBeenCalledWith({
        sourceId: "official",
        modId: "upscale_pack",
        version: "1.2.0",
      });
    });

    // Also verify local archive install forwards paths to preflight and aborts on cancel
    preflightSpy.mockClear();
    fireEvent.click(screen.getByRole("tab", { name: "Sources & Import" }));
    fireEvent.click(await screen.findByRole("button", { name: /Install from package/i }));
    await waitFor(() => {
      expect(preflightSpy).toHaveBeenCalledWith({
        kind: "mod",
        estimatedBytes: 0,
        paths: ["/mock/markdown_tools-0.1.0.rmod.tar.gz"],
      });
    });
    const archiveDialog = await screen.findByTestId("storage-preflight-dialog");
    fireEvent.click(within(archiveDialog).getByTestId("storage-preflight-cancel"));
    await waitFor(() => {
      expect(screen.queryByTestId("storage-preflight-dialog")).not.toBeInTheDocument();
    });
    expect(screen.queryByTestId("mod-install-consent-dialog")).not.toBeInTheDocument();
  });
});
