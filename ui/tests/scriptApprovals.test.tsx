import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { toast } from "sonner";
import { act, fireEvent, render, screen, waitFor, within } from "./test-utils";
import { ScriptApprovalBanner } from "../src/components/layout/ScriptApprovalBanner";
import { SettingsDialog } from "../src/components/settings/SettingsDialog";
import { extractModNamespace } from "../src/hooks/useOperations";
import { textOperationLabel } from "../src/lib/textOperations";
import {
  resetMockScriptApprovals,
  seedMockOnlinePrepare,
  seedMockScriptApproval,
} from "../src/ipc/mock";
import { scriptApprovalsSnapshot } from "../src/ipc";
import { useProjectStore } from "../src/store/project";
import { useUIStore } from "../src/store/uiStore";

vi.mock("sonner", () => ({
  toast: { error: vi.fn(), success: vi.fn(), info: vi.fn() },
  Toaster: () => null,
}));

const PROJECT = {
  id: "default",
  name: "Default",
  description: "Default workspace project",
  createdAt: "2026-08-18T00:00:00Z",
  updatedAt: "2026-08-18T00:00:00Z",
};

const DIGEST_A = "a".repeat(64);
const ENV_HASH_B = "b".repeat(64);

function makeQueryClient() {
  return new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
}

describe("Script approvals UI & kernel namespace", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    localStorage.clear();
    resetMockScriptApprovals();
    useProjectStore.setState({ projects: [PROJECT], activeProjectId: PROJECT.id });
    useUIStore.setState({
      settingsOpen: false,
      settingsSection: "general",
      settingsWorkspaceId: null,
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  test("treats script.* as a reserved kernel namespace with a localized label", () => {
    expect(extractModNamespace("script.python")).toBeNull();
    expect(extractModNamespace("text.replace")).toBe("text");
    expect(textOperationLabel("script.python")).toBe("Python Script");
  });

  test("ScriptApprovalBanner auto-opens dialog on live event and approves execution digest", async () => {
    const queryClient = makeQueryClient();
    render(
      <QueryClientProvider client={queryClient}>
        <ScriptApprovalBanner />
      </QueryClientProvider>,
    );

    // Wait for initial snapshot query and async mock event subscription to settle.
    await waitFor(() => {
      expect(queryClient.getQueryData(["script-approvals", "default"])).toBeDefined();
    });
    expect(screen.queryByTestId("script-approval-banner")).not.toBeInTheDocument();

    act(() => {
      seedMockScriptApproval({
        project_id: "default",
        exec_digest: DIGEST_A,
        code: "def main(ctx):\n    return {'ok': True}\n",
        python: "3.12",
        dependencies: ["pillow>=10.0"],
        allow_source_build: false,
        lock_sha256: null,
        offline_sandbox: true,
      });
    });

    const dialog = await screen.findByTestId("script-approval-dialog");
    expect(screen.getByTestId("script-approval-banner")).toBeInTheDocument();
    expect(within(dialog).getByTestId("script-approval-digest")).toHaveTextContent(DIGEST_A);
    expect(within(dialog).getByTestId("script-approval-code")).toHaveTextContent("def main(ctx):");
    expect(within(dialog).getByText("pillow>=10.0")).toBeInTheDocument();
    expect(within(dialog).getByText(/Offline sandbox \(seatbelt\)/)).toBeInTheDocument();

    fireEvent.click(within(dialog).getByTestId("script-approval-confirm"));

    await waitFor(() => {
      expect(screen.queryByTestId("script-approval-dialog")).not.toBeInTheDocument();
      expect(screen.queryByTestId("script-approval-banner")).not.toBeInTheDocument();
    });
    expect(toast.success).toHaveBeenCalled();

    const snapshot = await scriptApprovalsSnapshot("default");
    expect(snapshot.pending_executions).toHaveLength(0);
    expect(snapshot.approved_executions).toHaveLength(1);
    expect(snapshot.approved_executions[0]?.exec_digest).toBe(DIGEST_A);
  });

  test("online prepare review surfaces source-build warning and supports queue navigation & dismiss", async () => {
    seedMockOnlinePrepare({
      project_id: "default",
      env_hash: ENV_HASH_B,
      python: "3.12",
      dependencies: ["scipy>=1.14"],
      allow_source_build: true,
      no_build: false,
      missing_uv: true,
      missing_managed_python: true,
    });
    seedMockScriptApproval({
      project_id: "default",
      exec_digest: DIGEST_A,
      code: "print('queued')",
      python: "3.12",
      dependencies: [],
      allow_source_build: false,
      lock_sha256: null,
      offline_sandbox: true,
    });

    const queryClient = makeQueryClient();
    render(
      <QueryClientProvider client={queryClient}>
        <ScriptApprovalBanner />
      </QueryClientProvider>,
    );

    const reviewBtn = await screen.findByTestId("script-approval-banner-review");
    fireEvent.click(reviewBtn);

    const dialog = await screen.findByTestId("script-approval-dialog");
    expect(within(dialog).getByTestId("script-approval-queue-nav")).toHaveTextContent("1 of 2");
    expect(within(dialog).getByTestId("script-approval-source-build-warning")).toBeInTheDocument();
    expect(within(dialog).getByTestId("script-approval-digest")).toHaveTextContent(ENV_HASH_B);

    // Approve the online prepare first; dialog advances to the execution review.
    fireEvent.click(within(dialog).getByTestId("script-approval-confirm"));
    await waitFor(() => {
      expect(within(dialog).getByTestId("script-approval-digest")).toHaveTextContent(DIGEST_A);
    });

    // Dismiss the execution review; queue empties and closes cleanly.
    fireEvent.click(within(dialog).getByTestId("script-approval-dismiss"));
    await waitFor(() => {
      expect(screen.queryByTestId("script-approval-dialog")).not.toBeInTheDocument();
      expect(screen.queryByTestId("script-approval-banner")).not.toBeInTheDocument();
    });
    expect(toast.info).toHaveBeenCalled();
  });

  test("Workspace Settings Scripts section displays sandbox status, approves pending, and revokes digests", async () => {
    seedMockScriptApproval({
      project_id: "default",
      exec_digest: DIGEST_A,
      code: "print('approved via settings')",
      python: "3.12",
      dependencies: ["numpy>=2.0"],
      allow_source_build: false,
      lock_sha256: null,
      offline_sandbox: true,
    });

    const queryClient = makeQueryClient();
    render(
      <QueryClientProvider client={queryClient}>
        <SettingsDialog />
      </QueryClientProvider>,
    );

    act(() => {
      useUIStore.getState().openSettings("workspace-scripts");
    });

    const section = await screen.findByTestId("workspace-scripts-section");
    await waitFor(() => {
      expect(within(section).getByTestId("workspace-scripts-sandbox-chip")).toHaveTextContent(
        /seatbelt/,
      );
      expect(within(section).getAllByTestId("workspace-script-pending-row")).toHaveLength(1);
    });

    // Review & approve from inside Settings.
    fireEvent.click(within(section).getByRole("button", { name: "Review" }));
    const reviewDialog = await screen.findByTestId("script-approval-dialog");
    fireEvent.click(within(reviewDialog).getByTestId("script-approval-confirm"));

    await waitFor(() => {
      expect(within(section).getAllByTestId("workspace-script-approved-row")).toHaveLength(1);
    });

    // Revoke the persisted approval.
    fireEvent.click(within(section).getByTestId(`revoke-script-${DIGEST_A}`));
    await waitFor(() => {
      expect(within(section).queryAllByTestId("workspace-script-approved-row")).toHaveLength(0);
    });
    expect(toast.success).toHaveBeenCalled();
  });
});
