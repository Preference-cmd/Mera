import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { toast } from "sonner";
import { act, fireEvent, render, screen, waitFor, within } from "./test-utils";
import { CollectionView } from "../src/components/layout/CollectionView";
import { LibraryView } from "../src/components/layout/LibraryView";
import { isSingleTextDocumentOperation, useOperations } from "../src/hooks/useOperations";
import { textOperationLabel } from "../src/lib/textOperations";
import * as ipc from "../src/ipc";
import { mockOpInvoke, mockOpList, mockSubscribeEvents } from "../src/ipc/mock";
import type { Event, OperationInfo } from "../src/ipc/schemas";
import { useBoardStore } from "../src/store/board";
import { useProjectStore } from "../src/store/project";
import { useUIStore } from "../src/store/uiStore";

const mockNavigate = vi.fn();
let mockParams = { id: "col_demo_drafts" };
let mockSearch: { type?: string } = {};

vi.mock("sonner", () => ({
  toast: {
    error: vi.fn(),
    success: vi.fn(),
  },
}));

vi.mock("@tanstack/react-router", () => ({
  Link: ({
    to,
    params,
    children,
    ...props
  }: {
    to: string;
    params?: { id?: string };
    children: React.ReactNode;
    [key: string]: unknown;
  }) => (
    <a href={to.replace("$id", params?.id ?? "")} {...props}>
      {children}
    </a>
  ),
  useNavigate: () => mockNavigate,
  useParams: () => mockParams,
  useRouter: () => ({ history: { canGoBack: () => false, back: vi.fn() } }),
  useSearch: () => mockSearch,
}));

function renderWithQueryClient(ui: React.ReactElement) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  });
  return render(<QueryClientProvider client={client}>{ui}</QueryClientProvider>);
}

describe("Phase W-f: recipe freezing, operation guard, and workflow detail dialog", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    window.localStorage.clear();
    useUIStore.setState({ settingsOpen: false, commandPaletteOpen: false });
    mockParams = { id: "col_demo_drafts" };
    mockSearch = {};
    useProjectStore.setState({ activeProjectId: "default" });
    useBoardStore.getState().hydrate({
      id: "board-default",
      projectId: "default",
      version: 1,
      items: [],
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  test("isSingleTextDocumentOperation returns false for dynamic_workflow.<id> even with doc asset fields", () => {
    const dynamicOp: OperationInfo = {
      spec: {
        name: "dynamic_workflow.dwf_123",
        description: "My Frozen Workflow",
        input_schema: {
          type: "object",
          properties: {
            doc: { type: "string", format: "asset", contentMediaType: "text/*" },
            params: { type: "object" },
          },
          required: ["doc"],
        },
        output_schema: {
          type: "object",
          properties: {
            doc: { type: "string", format: "asset" },
          },
          required: ["doc"],
        },
        annotations: { read_only: false },
      },
      provider: { dynamic_workflow: { id: "dwf_123" } },
      health: { status: "available" },
    };

    expect(isSingleTextDocumentOperation(dynamicOp)).toBe(false);
    expect(textOperationLabel("dynamic_workflow.dwf_123", [dynamicOp])).toBe("My Frozen Workflow");
    expect(textOperationLabel("dynamic_workflow.dwf_123", [])).toBe("dynamic_workflow.dwf_123");
  });

  test("useOperations reloads op_list on dynamic_workflow.changed document event", async () => {
    let listener: ((event: Event) => void) | null = null;
    vi.spyOn(ipc, "subscribeEvents").mockImplementation((cb) => {
      listener = cb;
      return () => {
        listener = null;
      };
    });

    const opListSpy = vi.spyOn(ipc, "opList").mockResolvedValue([]);

    function Probe() {
      const { operations } = useOperations("default");
      return <div data-testid="op-count">{operations.length}</div>;
    }

    render(<Probe />);
    await waitFor(() => {
      expect(opListSpy).toHaveBeenCalledTimes(1);
    });

    await act(async () => {
      listener?.({
        type: "document",
        kind: "dynamic_workflow.changed",
        projectId: "default",
        documentId: "dwf_new",
        version: 1,
      });
    });

    await waitFor(() => {
      expect(opListSpy).toHaveBeenCalledTimes(2);
    });
  });

  test("freezes a slot recipe via dynamic_workflow.from_recipe, exposes dynamic_workflow.<id> in mock opList, and hides freeze for stale/empty recipes", async () => {
    const subscribers = new Set<(event: Event) => void>();
    await mockSubscribeEvents((event) => {
      for (const cb of subscribers) cb(event);
    });
    vi.spyOn(ipc, "subscribeEvents").mockImplementation((cb) => {
      subscribers.add(cb);
      return () => subscribers.delete(cb);
    });
    vi.spyOn(ipc, "opList").mockImplementation((projectId) => mockOpList({ projectId }));
    vi.spyOn(ipc, "readAssetPreview").mockResolvedValue({
      url: "txt-url",
      mediaType: "text/markdown",
      text: "Hello world",
    });

    // Apply a text.normalize step to slot-1 in mock mode so it has a real recipe
    const normRes = await mockOpInvoke({
      projectId: "default",
      name: "text.normalize",
      input: { doc: "ast_demo_style_guide", whitespace: true, newline: true },
    });
    const normAssetId = (normRes.output as { doc: string }).doc;
    await mockOpInvoke({
      projectId: "default",
      name: "collection.set_slot",
      input: { collection_id: "col_demo_drafts", slot_id: "slot-1", asset: normAssetId },
    });

    const opInvokeSpy = vi
      .spyOn(ipc, "opInvoke")
      .mockImplementation((projectId, name, input) => mockOpInvoke({ projectId, name, input }));

    renderWithQueryClient(<CollectionView />);

    await waitFor(() => {
      expect(screen.getByTestId("slot-card-slot-1")).toBeInTheDocument();
    });

    // 1. Empty slot (slot-2 has 0 steps) -> freeze button is hidden
    fireEvent.click(screen.getByTestId("slot-card-slot-2"));
    await waitFor(() => {
      expect(screen.getByText(/No steps yet/i)).toBeInTheDocument();
    });
    expect(screen.queryByTestId("recipe-freeze-open")).toBeNull();

    // 2. Non-empty non-stale slot (slot-1) -> freeze button appears
    fireEvent.click(screen.getByTestId("slot-card-slot-1"));
    const freezeOpenBtn = await screen.findByTestId("recipe-freeze-open");
    fireEvent.click(freezeOpenBtn);

    const nameInput = screen.getByLabelText("Workflow name");
    expect(nameInput).toHaveValue("Blog drafts (slot-1)");
    fireEvent.change(nameInput, { target: { value: "Style Cleanup" } });
    fireEvent.click(screen.getByTestId("recipe-freeze-submit"));

    await waitFor(() => {
      expect(opInvokeSpy).toHaveBeenCalledWith("default", "dynamic_workflow.from_recipe", {
        collection_id: "col_demo_drafts",
        slot_id: "slot-1",
        name: "Style Cleanup",
      });
    });

    await waitFor(() => {
      expect(toast.success).toHaveBeenCalledWith(
        "Workflow saved",
        expect.objectContaining({
          action: expect.objectContaining({ label: "View in library" }),
        }),
      );
    });

    // Saved workflow is now exposed in mockOpList as dynamic_workflow.<id>
    const opsAfter = await mockOpList({ projectId: "default" });
    const createdOp = opsAfter.find((op) => op.spec.description === "Style Cleanup");
    expect(createdOp).toBeDefined();
    expect(createdOp?.spec.name).toMatch(/^dynamic_workflow\.dwf_/);
    expect(isSingleTextDocumentOperation(createdOp)).toBe(false);

    // 3. Stale recipe -> freeze button is hidden
    opInvokeSpy.mockImplementation(async (projectId, name, input) => {
      if (name === "recipe.get") {
        return {
          step: {} as never,
          output: {
            collection_id: "col_demo_drafts",
            slot_id: "slot-3",
            root_asset: { id: "ast_demo_cover_brief", media_type: "text/plain", size: 10 },
            current_asset: { id: "ast_demo_cover_brief", media_type: "text/plain", size: 10 },
            stale: true,
            steps: [
              {
                step_id: "s_stale",
                op: "text.normalize",
                op_version: "1.0.0",
                input: { doc: "ast_demo_cover_brief" },
                input_sources: {},
                output: null,
                actor: { kind: "Human", id: null, label: null },
                read_only: false,
                stale: true,
              },
            ],
          },
        };
      }
      return mockOpInvoke({ projectId, name, input });
    });

    fireEvent.click(screen.getByTestId("slot-card-slot-3"));
    await waitFor(() => {
      expect(screen.getByTestId("recipe-step-s_stale")).toBeInTheDocument();
    });
    expect(screen.queryByTestId("recipe-freeze-open")).toBeNull();
  });

  test("clicking a workflow row in LibraryView opens DynamicWorkflowDetailDialog and renders steps from dynamic_workflow.get", async () => {
    vi.spyOn(ipc, "opList").mockImplementation((projectId) => mockOpList({ projectId }));
    const opInvokeSpy = vi
      .spyOn(ipc, "opInvoke")
      .mockImplementation((projectId, name, input) => mockOpInvoke({ projectId, name, input }));

    renderWithQueryClient(<LibraryView />);

    const workflowLink = await screen.findByRole("button", { name: "Draft cleanup" });
    expect(workflowLink).toHaveAttribute("data-testid", "library-workflow-link");
    fireEvent.click(workflowLink);

    const dialog = await screen.findByTestId("dynamic-workflow-detail-dialog");
    expect(opInvokeSpy).toHaveBeenCalledWith("default", "dynamic_workflow.get", {
      dynamic_workflow_id: "dwf_demo_cleanup",
    });

    await waitFor(() => {
      expect(within(dialog).getByText("Draft cleanup")).toBeInTheDocument();
      expect(
        within(dialog).getByText("dynamic_workflow.dwf_demo_cleanup · v3"),
      ).toBeInTheDocument();
      expect(within(dialog).getByTestId("workflow-detail-step-step_1")).toBeInTheDocument();
      expect(within(dialog).getByTestId("workflow-detail-step-step_2")).toBeInTheDocument();
      expect(within(dialog).getByTestId("workflow-detail-step-step_3")).toBeInTheDocument();
      expect(within(dialog).getByText("← inputs.doc")).toBeInTheDocument();
      expect(within(dialog).getAllByText("← steps.step_2.doc")).toHaveLength(2);
    });
  });
});
