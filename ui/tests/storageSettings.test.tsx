import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { ReactNode } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { toast } from "sonner";
import { act, fireEvent, render, screen, waitFor, within } from "./test-utils";
import { SettingsDialog } from "../src/components/settings/SettingsDialog";
import { CommandPalette } from "../src/components/palette/CommandPalette";
import { AppShell } from "../src/components/layout/AppShell";
import * as ipc from "../src/ipc";
import type { StorageStatusDto } from "../src/ipc/schemas";
import { resetMockStorage } from "../src/ipc/mock";
import { useProjectStore } from "../src/store/project";
import { useSettingsStore } from "../src/store/settingsStore";
import { useUIStore } from "../src/store/uiStore";

const mockNavigate = vi.fn();

vi.mock("sonner", () => ({
  toast: { error: vi.fn(), success: vi.fn(), info: vi.fn() },
  Toaster: () => null,
}));

vi.mock("@tanstack/react-router", () => ({
  Link: ({ to, children, ...props }: { to: string; children: ReactNode }) => (
    <a href={to} {...props}>
      {children}
    </a>
  ),
  Outlet: () => <div data-testid="mock-outlet" />,
  useLocation: () => ({ pathname: "/", searchStr: "" }),
  useNavigate: () => mockNavigate,
  useRouter: () => ({
    history: {
      subscribe: () => () => {},
      location: { state: { __TSR_index: 0 } },
      back: vi.fn(),
      forward: vi.fn(),
    },
  }),
  useRouterState: () => 0,
}));

const GIB = 1024 * 1024 * 1024;
const MIB = 1024 * 1024;

function makeStatus(overrides: Partial<StorageStatusDto> = {}): StorageStatusDto {
  return {
    totalBytes: 12 * GIB,
    limitBytes: 50 * GIB,
    diskTotalBytes: 500 * GIB,
    diskAvailableBytes: 250 * GIB,
    diskFloorRatio: 0.1,
    diskFloorBytes: 50 * GIB,
    overBudget: false,
    belowDiskFloor: false,
    showFiftyGbNudge: false,
    nudgeDismissed: false,
    scanRunning: false,
    categories: {
      libraryBytes: 2 * GIB,
      libraryRemovedBytes: 256 * MIB,
      modelsBytes: 8 * GIB,
      workersBytes: 512 * MIB,
      modsBytes: 128 * MIB,
      cacheBytes: 512 * MIB,
      projectsTotalBytes: 896 * MIB,
    },
    projects: [
      {
        projectId: "default",
        name: "Default",
        assetsBytes: 512 * MIB,
        historyBytes: 0,
        totalBytes: 512 * MIB,
      },
      {
        projectId: "proj-2",
        name: "Film Project",
        assetsBytes: 384 * MIB,
        historyBytes: 0,
        totalBytes: 384 * MIB,
      },
    ],
    ...overrides,
  };
}

const PROJECTS = [
  {
    id: "default",
    name: "Default",
    description: "",
    createdAt: "2026-08-18T00:00:00Z",
    updatedAt: "2026-08-18T00:00:00Z",
  },
  {
    id: "proj-2",
    name: "Film Project",
    description: "",
    createdAt: "2026-08-18T00:00:00Z",
    updatedAt: "2026-08-18T00:00:00Z",
  },
];

function makeQueryClient() {
  return new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
}

describe("Storage settings, AppShell banners, and CommandPalette", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    localStorage.clear();
    resetMockStorage();
    useUIStore.setState({
      settingsOpen: false,
      settingsSection: "general",
      settingsWorkspaceId: null,
      palette: { kind: "closed" },
      sessionPanelOpen: false,
    });
    useSettingsStore.setState({ sidebarCollapsed: false });
    useProjectStore.setState({ projects: PROJECTS, activeProjectId: "default" });
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  test("SettingsDialog lists Storage in App group and CommandPalette opens Storage settings", async () => {
    vi.spyOn(ipc, "storageStatus").mockResolvedValue(makeStatus());
    const qc = makeQueryClient();

    render(
      <QueryClientProvider client={qc}>
        <CommandPalette />
        <SettingsDialog />
      </QueryClientProvider>,
    );

    act(() => {
      useUIStore.getState().openCommandPalette();
    });

    const input = await screen.findByPlaceholderText(/Search commands/i);
    fireEvent.change(input, { target: { value: "storage" } });
    const item = await screen.findByRole("button", { name: /Open Storage Settings/i });
    fireEvent.click(item);

    const dialog = await screen.findByTestId("settings-dialog");
    expect(within(dialog).getByRole("heading", { name: "Storage" })).toBeInTheDocument();
    const storageNavBtn = within(dialog).getByRole("button", { name: "Storage" });
    expect(storageNavBtn).toHaveAttribute("aria-current", "page");
    await waitFor(() => expect(storageNavBtn).toHaveFocus());
  });

  test("StorageSettingsSection renders breakdown, edits budget, cleans cache, and navigates", async () => {
    const initial = makeStatus();
    const afterPreset = makeStatus({ limitBytes: 20 * GIB });
    const afterBudget = makeStatus({ limitBytes: 80 * GIB });
    const afterUnlimited = makeStatus({ limitBytes: null });
    const afterClean = makeStatus({
      totalBytes: 11.5 * GIB,
      categories: { ...initial.categories, cacheBytes: 0 },
    });

    vi.spyOn(ipc, "storageStatus").mockResolvedValue(initial);
    const setBudgetSpy = vi
      .spyOn(ipc, "storageSetBudget")
      .mockResolvedValueOnce(afterPreset)
      .mockResolvedValueOnce(afterBudget)
      .mockResolvedValueOnce(afterUnlimited);
    const cleanCacheSpy = vi.spyOn(ipc, "storageCleanCache").mockResolvedValue({
      freedBytes: 512 * MIB,
      status: afterClean,
    });

    const qc = makeQueryClient();
    act(() => {
      useUIStore.getState().openSettings("storage");
    });
    render(
      <QueryClientProvider client={qc}>
        <SettingsDialog />
      </QueryClientProvider>,
    );

    await screen.findByTestId("storage-settings-section");
    expect(screen.getByTestId("storage-usage-summary")).toHaveTextContent("12 GBof 50 GB");
    expect(screen.getByTestId("storage-segmented-bar")).toBeInTheDocument();
    expect(screen.getByTestId("storage-cache-size")).toHaveTextContent("512 MB");
    expect(screen.getByTestId("storage-library-size")).toHaveTextContent("2 GB");
    expect(screen.getByTestId("storage-models-size")).toHaveTextContent("8 GB");
    expect(screen.getByTestId("storage-workers-size")).toHaveTextContent("512 MB");
    expect(screen.getByTestId("storage-mods-size")).toHaveTextContent("128 MB");
    expect(screen.getByText(/Includes 256 MB in removed items/i)).toBeInTheDocument();
    expect(screen.getByText(/File history snapshots are not enabled yet/i)).toBeInTheDocument();
    expect(screen.getByText("10% safety floor (50 GB)")).toBeInTheDocument();
    expect(screen.getByText("Current")).toBeInTheDocument();

    // Preset pill sets limitBytes immediately
    fireEvent.click(screen.getByTestId("storage-budget-preset-20"));
    await waitFor(() => expect(setBudgetSpy).toHaveBeenCalledWith(20 * GIB));

    // Invalid budget value shows inline error and does not call storageSetBudget
    const budgetInput = screen.getByTestId("storage-budget-input");
    fireEvent.change(budgetInput, { target: { value: "-5" } });
    fireEvent.keyDown(budgetInput, { key: "Enter" });
    expect(await screen.findByRole("alert")).toHaveTextContent(/positive number/i);
    expect(setBudgetSpy).toHaveBeenCalledTimes(1);

    // Valid budget value calls storageSetBudget immediately on Enter
    fireEvent.change(budgetInput, { target: { value: "80" } });
    fireEvent.keyDown(budgetInput, { key: "Enter" });
    await waitFor(() => expect(setBudgetSpy).toHaveBeenCalledWith(80 * GIB));

    // Clicking Unlimited sets limitBytes = null
    fireEvent.click(screen.getByTestId("storage-budget-unlimited"));
    await waitFor(() => expect(setBudgetSpy).toHaveBeenCalledWith(null));

    // Clean cache button frees cache and shows toast
    fireEvent.click(screen.getByTestId("storage-clean-cache-btn"));
    await waitFor(() => expect(cleanCacheSpy).toHaveBeenCalledTimes(1));
    expect(toast.success).toHaveBeenCalledWith(expect.stringContaining("512 MB"));

    // View by size closes settings and navigates to /assets?sort=size
    fireEvent.click(screen.getByTestId("storage-library-view-by-size"));
    expect(useUIStore.getState().settingsOpen).toBe(false);
    expect(mockNavigate).toHaveBeenCalledWith({ to: "/assets", search: { sort: "size" } });

    // Reopen and test Manage models
    act(() => {
      useUIStore.getState().openSettings("storage");
    });
    fireEvent.click(await screen.findByTestId("storage-manage-models"));
    expect(useUIStore.getState().settingsOpen).toBe(false);
    expect(mockNavigate).toHaveBeenCalledWith({ to: "/models" });

    // Reopen and test Manage mods closes settings and navigates to /mods
    act(() => {
      useUIStore.getState().openSettings("storage");
    });
    fireEvent.click(await screen.findByTestId("storage-manage-mods"));
    expect(useUIStore.getState().settingsOpen).toBe(false);
    expect(mockNavigate).toHaveBeenCalledWith({ to: "/mods" });
  });

  test("StorageSettingsSection supports manual library.purge and project delete confirmation", async () => {
    const status = makeStatus();
    const statusSpy = vi.spyOn(ipc, "storageStatus").mockResolvedValue(status);
    const opInvokeSpy = vi.spyOn(ipc, "opInvoke").mockResolvedValue({
      step: { id: "s1" },
      output: { purged_entries: 3, deleted_files: 2, freed_bytes: 256 * MIB, version: 2 },
    });
    const deleteProjectSpy = vi.spyOn(ipc, "deleteProject").mockResolvedValue(undefined);
    vi.spyOn(ipc, "listProjects").mockResolvedValue([PROJECTS[0]]);

    const qc = makeQueryClient();
    act(() => {
      useUIStore.getState().openSettings("storage");
    });
    render(
      <QueryClientProvider client={qc}>
        <SettingsDialog />
      </QueryClientProvider>,
    );

    await screen.findByTestId("storage-settings-section");

    // Purge removed items confirmation flow
    fireEvent.click(screen.getByTestId("storage-library-purge-btn"));
    const purgeDialog = await screen.findByTestId("storage-purge-confirm-dialog");
    expect(within(purgeDialog).getByText(/256 MB/)).toBeInTheDocument();
    fireEvent.click(within(purgeDialog).getByTestId("storage-purge-confirm-btn"));

    await waitFor(() => {
      expect(opInvokeSpy).toHaveBeenCalledWith("default", "library.purge", {});
    });
    expect(toast.success).toHaveBeenCalledWith(expect.stringContaining("256 MB"));
    expect(statusSpy.mock.calls.length).toBeGreaterThanOrEqual(2);

    // Delete project confirmation flow
    fireEvent.click(screen.getByTestId("storage-delete-project-proj-2"));
    const deleteDialog = await screen.findByTestId("storage-delete-project-dialog");
    expect(within(deleteDialog).getByText(/Film Project/)).toBeInTheDocument();
    fireEvent.click(within(deleteDialog).getByTestId("storage-delete-project-confirm-btn"));

    await waitFor(() => {
      expect(deleteProjectSpy).toHaveBeenCalledWith("proj-2");
    });
    expect(useProjectStore.getState().projects.map((p) => p.id)).toEqual(["default"]);
  });

  test("AppShell shows persistent warning banner when overBudget or belowDiskFloor", async () => {
    vi.spyOn(ipc, "storageStatus").mockResolvedValue(
      makeStatus({
        totalBytes: 55 * GIB,
        limitBytes: 50 * GIB,
        overBudget: true,
        belowDiskFloor: false,
      }),
    );

    const qc = makeQueryClient();
    render(
      <QueryClientProvider client={qc}>
        <AppShell />
      </QueryClientProvider>,
    );

    const banner = await screen.findByTestId("storage-warning-banner");
    expect(banner).toHaveTextContent(/55 GB/);
    expect(banner).toHaveTextContent(/50 GB/);

    // Persistent: no dismiss button
    expect(within(banner).queryByTestId("storage-nudge-dismiss")).not.toBeInTheDocument();

    // Clicking action opens storage settings
    fireEvent.click(screen.getByTestId("storage-warning-action"));
    expect(useUIStore.getState().settingsOpen).toBe(true);
    expect(useUIStore.getState().settingsSection).toBe("storage");
  });

  test("AppShell shows dismissible 50GB nudge and calls storageDismissNudge on dismiss", async () => {
    const nudgeStatus = makeStatus({
      totalBytes: 54 * GIB,
      limitBytes: null,
      overBudget: false,
      belowDiskFloor: false,
      showFiftyGbNudge: true,
      nudgeDismissed: false,
    });
    const dismissedStatus = makeStatus({
      totalBytes: 54 * GIB,
      limitBytes: null,
      showFiftyGbNudge: false,
      nudgeDismissed: true,
    });

    vi.spyOn(ipc, "storageStatus")
      .mockResolvedValueOnce(nudgeStatus)
      .mockResolvedValue(dismissedStatus);
    const dismissSpy = vi.spyOn(ipc, "storageDismissNudge").mockResolvedValue(dismissedStatus);

    const qc = makeQueryClient();
    render(
      <QueryClientProvider client={qc}>
        <AppShell />
      </QueryClientProvider>,
    );

    const nudge = await screen.findByTestId("storage-nudge-banner");
    expect(nudge).toHaveTextContent(/54 GB/);

    // Action button opens storage settings
    fireEvent.click(screen.getByTestId("storage-nudge-action"));
    expect(useUIStore.getState().settingsOpen).toBe(true);
    expect(useUIStore.getState().settingsSection).toBe("storage");

    // Dismiss button calls storageDismissNudge and removes the banner
    fireEvent.click(screen.getByTestId("storage-nudge-dismiss"));
    await waitFor(() => {
      expect(dismissSpy).toHaveBeenCalledTimes(1);
    });
    await waitFor(() => {
      expect(screen.queryByTestId("storage-nudge-banner")).not.toBeInTheDocument();
    });
  });
});
