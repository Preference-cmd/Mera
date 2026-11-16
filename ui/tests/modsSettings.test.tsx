import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, fireEvent, render, renderHook, screen, waitFor, within } from "./test-utils";
import { SettingsDialog } from "../src/components/settings/SettingsDialog";
import { SlotRecipePanel } from "../src/components/layout/SlotRecipePanel";
import { useOperations } from "../src/hooks/useOperations";
import * as ipc from "../src/ipc";
import {
  mockModsInstallArchive,
  mockModsInstallDirectory,
  mockModsSetAgentVisibility,
  mockModsSetEnabled,
  resetMockMods,
  stageMockLocalModUpgrade,
} from "../src/ipc/mock";
import { useProjectStore } from "../src/store/project";
import { useUIStore } from "../src/store/uiStore";

vi.mock("sonner", () => ({
  toast: { error: vi.fn(), success: vi.fn(), info: vi.fn() },
}));

vi.mock("@tanstack/react-router", () => ({
  useNavigate: () => vi.fn(),
}));

const PROJECT = {
  id: "default",
  name: "Default",
  description: "Default workspace project",
  createdAt: "2026-08-18T00:00:00Z",
  updatedAt: "2026-08-18T00:00:00Z",
};

function renderSettingsAtMods() {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  act(() => {
    useUIStore.getState().openSettings("mods");
  });
  return render(
    <QueryClientProvider client={queryClient}>
      <SettingsDialog />
    </QueryClientProvider>,
  );
}

describe("Settings -> App -> Mods & M2 consent flow", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    resetMockMods();
    localStorage.clear();
    useUIStore.setState({
      settingsOpen: false,
      settingsSection: "general",
      settingsWorkspaceId: null,
    });
    useProjectStore.setState({ projects: [PROJECT], activeProjectId: PROJECT.id });
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  test("1. displays mods_root and bundled text mod as read-only always active", async () => {
    renderSettingsAtMods();

    const dialog = await screen.findByTestId("settings-dialog");
    expect(within(dialog).getByRole("heading", { name: "Mods" })).toBeInTheDocument();
    expect(await within(dialog).findByText("/mock/.mera/mods")).toBeInTheDocument();

    const textCard = await within(dialog).findByTestId("mod-card-text");
    expect(within(textCard).getAllByText("Always active").length).toBeGreaterThan(0);

    const switchBtn = within(textCard).getByRole("switch", { name: /Enable or disable Text Mod/i });
    expect(switchBtn).toBeDisabled();
    expect(switchBtn).toHaveAttribute("aria-checked", "true");
    expect(within(textCard).queryByRole("button", { name: "Uninstall" })).not.toBeInTheDocument();

    const checkboxes = within(textCard).getAllByRole("checkbox");
    expect(checkboxes.length).toBeGreaterThanOrEqual(6);
    for (const cb of checkboxes) {
      expect(cb).toBeChecked();
      expect(cb).toBeDisabled();
    }
  });

  test("2. package install consent keeps all Agent operations unchecked by default (D2)", async () => {
    renderSettingsAtMods();
    const dialog = await screen.findByTestId("settings-dialog");

    const installBtn = await within(dialog).findByRole("button", { name: /Install from package/i });
    fireEvent.click(installBtn);

    const consentDialog = await screen.findByTestId("mod-install-consent-dialog");
    expect(
      within(consentDialog).getByText(
        /Local and third-party mods run as local child processes without an OS sandbox/i,
      ),
    ).toBeInTheDocument();
    expect(within(consentDialog).getByTestId("mod-consent-sha256")).toHaveTextContent(
      "a".repeat(64),
    );
    expect(within(consentDialog).getByText("Author recommended")).toBeInTheDocument();

    const calloutBox = within(consentDialog).getByRole("checkbox", {
      name: /Expose to Agent: markdown_tools\.callout/i,
    });
    const lintBox = within(consentDialog).getByRole("checkbox", {
      name: /Expose to Agent: markdown_tools\.lint/i,
    });
    expect(calloutBox).not.toBeChecked();
    expect(lintBox).not.toBeChecked();

    fireEvent.click(within(consentDialog).getByRole("button", { name: "Install mod" }));

    await waitFor(() => {
      expect(screen.queryByTestId("mod-install-consent-dialog")).not.toBeInTheDocument();
    });

    const card = await within(dialog).findByTestId("mod-card-markdown_tools");
    expect(within(card).getByTestId("mod-agent-count-markdown_tools")).toHaveTextContent(
      "Agent: 0/2",
    );
  });

  test("3. toggling per-operation Agent checkbox and enable switch updates state", async () => {
    await mockModsInstallArchive({
      path: "/mock/markdown_tools-0.1.0.rmod.tar.gz",
      consentGranted: true,
      consentPermissionExpansion: false,
      enabled: true,
      agentVisibleOperations: [],
    });

    const visSpy = vi.spyOn(ipc, "modsSetAgentVisibility");
    const enabledSpy = vi.spyOn(ipc, "modsSetEnabled");

    renderSettingsAtMods();
    const card = await screen.findByTestId("mod-card-markdown_tools");
    expect(within(card).getByTestId("mod-agent-count-markdown_tools")).toHaveTextContent(
      "Agent: 0/2",
    );

    const calloutBox = within(card).getByRole("checkbox", {
      name: /Expose to Agent: markdown_tools\.callout/i,
    });
    fireEvent.click(calloutBox);

    await waitFor(() => {
      expect(visSpy).toHaveBeenCalledWith("markdown_tools", ["markdown_tools.callout"]);
    });
    await waitFor(() => {
      expect(within(card).getByTestId("mod-agent-count-markdown_tools")).toHaveTextContent(
        "Agent: 1/2",
      );
    });

    const switchBtn = within(card).getByRole("switch", {
      name: /Enable or disable Markdown Tools/i,
    });
    fireEvent.click(switchBtn);
    await waitFor(() => {
      expect(enabledSpy).toHaveBeenCalledWith("markdown_tools", false);
    });
    await waitFor(() => {
      expect(within(card).getByText("Disabled")).toBeInTheDocument();
    });
  });

  test("4. detects source conflicts and handles local-dir reload permission expansion consent", async () => {
    await mockModsInstallArchive({
      path: "/mock/markdown_tools-0.1.0.rmod.tar.gz",
      consentGranted: true,
      consentPermissionExpansion: false,
      enabled: true,
      agentVisibleOperations: [],
    });

    renderSettingsAtMods();
    const dialog = await screen.findByTestId("settings-dialog");

    // Trying to mount a dev directory when already installed as archive shows conflict blocker
    fireEvent.click(within(dialog).getByRole("button", { name: /Mount dev folder/i }));
    const conflictDialog = await screen.findByTestId("mod-install-consent-dialog");
    expect(within(conflictDialog).getByTestId("mod-consent-source-conflict")).toBeInTheDocument();
    expect(
      within(conflictDialog).getByRole("button", { name: "Approve and upgrade" }),
    ).toBeDisabled();
    fireEvent.click(within(conflictDialog).getByRole("button", { name: "Cancel" }));

    // Replace cleanly with a local directory mount carrying one opted-in op
    resetMockMods();
    await mockModsInstallDirectory({
      path: "/mock/dev/markdown_tools",
      consentGranted: true,
      consentPermissionExpansion: false,
      enabled: true,
      agentVisibleOperations: ["markdown_tools.callout"],
    });
    stageMockLocalModUpgrade("markdown_tools", {
      version: "0.2.0",
      network: true,
      addRemoteOp: true,
    });

    // Trigger refresh of mods list via reload/event
    await mockModsSetAgentVisibility({
      modId: "markdown_tools",
      operations: ["markdown_tools.callout"],
    });

    const card = await within(dialog).findByTestId("mod-card-markdown_tools");
    const reloadBtn = await within(card).findByRole("button", { name: "Reload" });
    fireEvent.click(reloadBtn);

    const consentDialog = await screen.findByTestId("mod-install-consent-dialog");
    expect(
      within(consentDialog).getByTestId("mod-consent-permission-expansions"),
    ).toHaveTextContent("network access requested");

    const calloutBox = within(consentDialog).getByRole("checkbox", {
      name: /Expose to Agent: markdown_tools\.callout/i,
    });
    const exportBox = within(consentDialog).getByRole("checkbox", {
      name: /Expose to Agent: markdown_tools\.export_remote/i,
    });
    expect(calloutBox).toBeChecked();
    expect(exportBox).not.toBeChecked();
    expect(within(consentDialog).getByText("New")).toBeInTheDocument();

    fireEvent.click(within(consentDialog).getByRole("button", { name: "Approve and reload" }));

    await waitFor(() => {
      expect(screen.queryByTestId("mod-install-consent-dialog")).not.toBeInTheDocument();
    });
    await waitFor(() => {
      expect(within(card).getByTestId("mod-agent-count-markdown_tools")).toHaveTextContent(
        "Agent: 1/3",
      );
    });
  });

  test("5. useOperations refreshes live when mod.* events fire", async () => {
    const { result } = renderHook(() => useOperations("default"));

    await waitFor(() => {
      expect(result.current.loading).toBe(false);
    });
    expect(
      result.current.textOperations.some((op) => op.spec.name === "markdown_tools.callout"),
    ).toBe(false);

    await act(async () => {
      await mockModsInstallArchive({
        path: "/mock/markdown_tools-0.1.0.rmod.tar.gz",
        consentGranted: true,
        consentPermissionExpansion: false,
        enabled: true,
        agentVisibleOperations: [],
      });
    });

    await waitFor(() => {
      expect(
        result.current.textOperations.some((op) => op.spec.name === "markdown_tools.callout"),
      ).toBe(true);
    });

    await act(async () => {
      await mockModsSetEnabled({ modId: "markdown_tools", enabled: false });
    });

    await waitFor(() => {
      expect(
        result.current.textOperations.some((op) => op.spec.name === "markdown_tools.callout"),
      ).toBe(false);
    });
  });

  test("6. SlotRecipePanel warns when a step mod is missing, keeps data intact, and opens Mods settings", async () => {
    vi.spyOn(ipc, "opInvoke").mockImplementation(async (_projectId, name) => {
      if (name === "recipe.get") {
        return {
          step: {} as never,
          output: {
            collection_id: "col_1",
            slot_id: "slot_1",
            root_asset: { id: "ast_root", media_type: "text/markdown", size: 24 },
            current_asset: { id: "ast_current", media_type: "text/markdown", size: 42 },
            stale: false,
            steps: [
              {
                step_id: "st_custom",
                op: "custom_mod.transform",
                op_version: "0.1.0",
                input: { doc: "ast_root" },
                actor: { kind: "Human", id: null, label: null },
                read_only: false,
                stale: false,
              },
            ],
          },
        };
      }
      return { step: {} as never, output: {} };
    });

    render(
      <SlotRecipePanel
        projectId="default"
        collectionId="col_1"
        slotId="slot_1"
        slotAssetId="ast_current"
        collectionVersion={1}
        availableOperations={[]}
        allOperations={[]}
        operationsLoading={false}
        onClose={vi.fn()}
        onEditStep={vi.fn()}
      />,
    );

    const stepRow = await screen.findByTestId("recipe-step-st_custom");
    expect(within(stepRow).getByText("custom_mod.transform")).toBeInTheDocument();

    const missingNotice = within(stepRow).getByTestId("recipe-step-missing-mod-st_custom");
    expect(missingNotice).toHaveTextContent('Mod "custom_mod" is missing or disabled');
    expect(screen.getByText("ast_current")).toBeInTheDocument();

    fireEvent.click(within(missingNotice).getByRole("button", { name: "Manage mods" }));
    expect(useUIStore.getState().settingsOpen).toBe(true);
    expect(useUIStore.getState().settingsSection).toBe("mods");
  });
});
