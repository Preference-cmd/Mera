import { describe, test, expect, vi, beforeEach } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { toast } from "sonner";
import { act, fireEvent, render, screen, waitFor } from "./test-utils";
import { WorkspaceFoldersDialog } from "../src/components/layout/WorkspaceFoldersDialog";
import * as ipc from "../src/ipc";

vi.mock("sonner", () => ({
  toast: {
    error: vi.fn(),
    success: vi.fn(),
    info: vi.fn(),
  },
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn().mockResolvedValue("/mock/new-workspace-folder"),
}));

const mockFolders = [
  { id: "f-1", path: "/mock/workspace/photos" },
  { id: "f-2", path: "/mock/workspace/docs" },
];

describe("WorkspaceFoldersDialog", () => {
  let queryClient: QueryClient;

  beforeEach(() => {
    vi.clearAllMocks();
    vi.spyOn(ipc, "isTauri").mockReturnValue(true);
    queryClient = new QueryClient({
      defaultOptions: {
        queries: { retry: false, gcTime: 0 },
        mutations: { retry: false },
      },
    });
  });

  function renderDialog(open = true, onOpenChange = vi.fn()) {
    return render(
      <QueryClientProvider client={queryClient}>
        <WorkspaceFoldersDialog open={open} onOpenChange={onOpenChange} projectId="test-project" />
      </QueryClientProvider>,
    );
  }

  test("renders folder list from project.list_folders", async () => {
    vi.spyOn(ipc, "opInvoke").mockImplementation(async (_projectId, name) => {
      if (name === "project.list_folders") {
        return { step: {}, output: { folders: mockFolders } };
      }
      return { step: {}, output: {} };
    });

    renderDialog();

    await waitFor(() => {
      expect(screen.getByText("/mock/workspace/photos")).toBeInTheDocument();
      expect(screen.getByText("/mock/workspace/docs")).toBeInTheDocument();
    });
  });

  test("adds a folder when user selects directory", async () => {
    const invokedOps: { name: string; input?: unknown }[] = [];
    vi.spyOn(ipc, "opInvoke").mockImplementation(async (_projectId, name, input) => {
      invokedOps.push({ name, input });
      if (name === "project.list_folders") {
        return { step: {}, output: { folders: mockFolders } };
      }
      if (name === "project.add_folder") {
        return {
          step: {},
          output: { folder: { id: "f-3", path: "/mock/new-workspace-folder" } },
        };
      }
      return { step: {}, output: {} };
    });

    renderDialog();

    await waitFor(() => {
      expect(screen.getByText("/mock/workspace/photos")).toBeInTheDocument();
    });

    const addBtn = screen.getByRole("button", { name: /Add folder/i });
    await act(async () => {
      fireEvent.click(addBtn);
    });

    await waitFor(() => {
      expect(invokedOps.some((op) => op.name === "project.add_folder")).toBe(true);
      expect(toast.success).toHaveBeenCalled();
    });
  });

  test("removes a folder with confirmation", async () => {
    vi.spyOn(window, "confirm").mockReturnValue(true);
    const invokedOps: { name: string; input?: unknown }[] = [];
    vi.spyOn(ipc, "opInvoke").mockImplementation(async (_projectId, name, input) => {
      invokedOps.push({ name, input });
      if (name === "project.list_folders") {
        return { step: {}, output: { folders: mockFolders } };
      }
      if (name === "project.remove_folder") {
        return { step: {}, output: {} };
      }
      return { step: {}, output: {} };
    });

    renderDialog();

    await waitFor(() => {
      expect(screen.getByText("/mock/workspace/photos")).toBeInTheDocument();
    });

    const removeButtons = screen.getAllByRole("button", { name: "Remove" });
    expect(removeButtons.length).toBeGreaterThan(0);
    await act(async () => {
      fireEvent.click(removeButtons[0]!);
    });

    await waitFor(() => {
      expect(
        invokedOps.some(
          (op) => op.name === "project.remove_folder" && (op.input as { id: string }).id === "f-1",
        ),
      ).toBe(true);
      expect(toast.success).toHaveBeenCalled();
    });
  });

  test("surfaces backend diagnostic errors when folder add fails", async () => {
    vi.spyOn(ipc, "opInvoke").mockImplementation(async (_projectId, name) => {
      if (name === "project.list_folders") {
        return { step: {}, output: { folders: mockFolders } };
      }
      if (name === "project.add_folder") {
        return {
          step: {
            diagnostics: [
              {
                severity: "error",
                message: "Folder cannot be inside application data directory",
              },
            ],
          },
          output: {},
        };
      }
      return { step: {}, output: {} };
    });

    renderDialog();

    await waitFor(() => {
      expect(screen.getByText("/mock/workspace/photos")).toBeInTheDocument();
    });

    const addBtn = screen.getByRole("button", { name: /Add folder/i });
    await act(async () => {
      fireEvent.click(addBtn);
    });

    await waitFor(() => {
      expect(toast.error).toHaveBeenCalledWith(
        "Folder cannot be inside application data directory",
      );
    });
  });
});
