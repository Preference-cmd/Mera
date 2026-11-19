import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, fireEvent, render, renderHook, screen, waitFor, within } from "./test-utils";
import { ModsView } from "../src/components/layout/ModsView";
import { SlotRecipePanel } from "../src/components/layout/SlotRecipePanel";
import { useOperations } from "../src/hooks/useOperations";
import * as ipc from "../src/ipc";
import {
  mockModsInstallArchive,
  mockModsInstallDirectory,
  mockModsSetAgentVisibility,
  mockModsSetEnabled,
  resetMockMods,
  setMockModDownloadDelay,
  stageMockLocalModUpgrade,
} from "../src/ipc/mock";
import { useProjectStore } from "../src/store/project";
import { useUIStore } from "../src/store/uiStore";

vi.mock("sonner", () => ({
  toast: { error: vi.fn(), success: vi.fn(), info: vi.fn() },
}));

const mockNavigate = vi.fn();

vi.mock("@tanstack/react-router", () => ({
  useNavigate: () => mockNavigate,
}));

const PROJECT = {
  id: "default",
  name: "Default",
  description: "Default workspace project",
  createdAt: "2026-08-18T00:00:00Z",
  updatedAt: "2026-08-18T00:00:00Z",
};

function renderModsView() {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  return render(
    <QueryClientProvider client={queryClient}>
      <ModsView />
    </QueryClientProvider>,
  );
}

describe("Sidebar -> ModsView & M2 consent flow", () => {
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

  test("1. displays mods_root in Sources tab and bundled text mod as read-only always active", async () => {
    renderModsView();

    const view = await screen.findByTestId("mods-view");
    expect(within(view).getByRole("heading", { name: "Mods" })).toBeInTheDocument();

    const textCard = await within(view).findByTestId("mod-card-text");
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

    fireEvent.click(within(view).getByRole("tab", { name: "Sources & Import" }));
    expect(await within(view).findByText("/mock/.mera/mods")).toBeInTheDocument();
  });

  test("2. package install consent keeps all Agent operations unchecked by default (D2)", async () => {
    renderModsView();
    const view = await screen.findByTestId("mods-view");

    fireEvent.click(within(view).getByRole("tab", { name: "Sources & Import" }));
    const installBtn = await within(view).findByRole("button", { name: /Install from package/i });
    fireEvent.click(installBtn);

    const consentDialog = await screen.findByTestId("mod-install-consent-dialog");
    expect(
      within(consentDialog).getByText(
        /Runs inside a seatbelt OS sandbox scoped to the permissions/i,
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

    fireEvent.click(within(view).getByRole("tab", { name: "Installed" }));
    const card = await within(view).findByTestId("mod-card-markdown_tools");
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

    renderModsView();
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

    renderModsView();
    const view = await screen.findByTestId("mods-view");

    // Trying to mount a dev directory when already installed as archive shows conflict blocker
    fireEvent.click(within(view).getByRole("tab", { name: "Sources & Import" }));
    fireEvent.click(await within(view).findByRole("button", { name: /Mount dev folder/i }));
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

    fireEvent.click(within(view).getByRole("tab", { name: "Installed" }));
    const card = await within(view).findByTestId("mod-card-markdown_tools");
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

  test("6. SlotRecipePanel warns when a step mod is missing, keeps data intact, and navigates to /mods", async () => {
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
    expect(mockNavigate).toHaveBeenCalledWith({ to: "/mods" });
  });

  test("7. Browse official registry installs directly with D2 default-hidden Agent operations", async () => {
    renderModsView();
    const view = await screen.findByTestId("mods-view");

    fireEvent.click(within(view).getByRole("tab", { name: "Browse" }));
    const jsonCard = await within(view).findByTestId("browse-mod-json_tools");
    fireEvent.click(within(jsonCard).getByRole("button", { name: "Install" }));

    await waitFor(() => {
      expect(within(jsonCard).getByRole("button", { name: "Installed" })).toBeDisabled();
    });

    fireEvent.click(within(view).getByRole("tab", { name: "Installed" }));
    const installedCard = await within(view).findByTestId("mod-card-json_tools");
    expect(within(installedCard).getByText("OS sandbox: seatbelt")).toBeInTheDocument();
    expect(within(installedCard).getByTestId("mod-agent-count-json_tools")).toHaveTextContent(
      "Agent: 0/1",
    );
  });

  test("8. Third-party remote source install downloads first, requires explicit consent, and supports uv badge", async () => {
    renderModsView();
    const view = await screen.findByTestId("mods-view");

    fireEvent.click(within(view).getByRole("tab", { name: "Sources & Import" }));
    fireEvent.change(within(view).getByLabelText("Source name"), {
      target: { value: "Community" },
    });
    fireEvent.change(within(view).getByLabelText("Index or GitHub Release URL (HTTPS)"), {
      target: { value: "https://community.example.com/index.json" },
    });
    fireEvent.click(within(view).getByRole("button", { name: "Add source" }));

    expect(await within(view).findByTestId("third-party-source-src-community")).toBeInTheDocument();

    fireEvent.click(within(view).getByRole("tab", { name: "Browse" }));
    const mdCard = await within(view).findByTestId("browse-mod-markdown_tools");
    fireEvent.click(within(mdCard).getByRole("button", { name: "Install" }));

    // Third-party remote package must pause at awaiting_consent and show ConsentDialog with SHA-256
    const consentDialog = await screen.findByTestId("mod-install-consent-dialog");
    expect(within(consentDialog).getByTestId("mod-consent-sha256")).toHaveTextContent(
      "d".repeat(64),
    );
    const calloutBox = within(consentDialog).getByRole("checkbox", {
      name: /Expose to Agent: markdown_tools\.callout/i,
    });
    expect(calloutBox).not.toBeChecked();
    fireEvent.click(calloutBox);
    fireEvent.click(within(consentDialog).getByRole("button", { name: "Install mod" }));

    await waitFor(() => {
      expect(screen.queryByTestId("mod-install-consent-dialog")).not.toBeInTheDocument();
    });

    fireEvent.click(within(view).getByRole("tab", { name: "Installed" }));
    const installedCard = await within(view).findByTestId("mod-card-markdown_tools");
    expect(within(installedCard).getByText("uv script")).toBeInTheDocument();
    expect(within(installedCard).getByTestId("mod-agent-count-markdown_tools")).toHaveTextContent(
      "Agent: 1/2",
    );
  });

  test("9. Cancelling an in-flight download aborts without installing and portable uv can be bootstrapped", async () => {
    setMockModDownloadDelay(400);
    renderModsView();
    const view = await screen.findByTestId("mods-view");

    fireEvent.click(within(view).getByRole("tab", { name: "Browse" }));
    const jsonCard = await within(view).findByTestId("browse-mod-json_tools");
    fireEvent.click(within(jsonCard).getByRole("button", { name: "Install" }));

    const activeRow = await within(view).findByTestId("mod-download-json_tools");
    fireEvent.click(within(activeRow).getByRole("button", { name: "Cancel" }));

    await waitFor(() => {
      expect(within(view).queryByTestId("mod-download-json_tools")).not.toBeInTheDocument();
    });

    fireEvent.click(within(view).getByRole("tab", { name: "Installed" }));
    expect(within(view).queryByTestId("mod-card-json_tools")).not.toBeInTheDocument();

    fireEvent.click(within(view).getByRole("tab", { name: "Sources & Import" }));
    const uvBtn = await within(view).findByRole("button", { name: "Install portable uv" });
    fireEvent.click(uvBtn);
    expect(await within(view).findByText(/Available \(managed_portable\)/i)).toBeInTheDocument();
  });
});
