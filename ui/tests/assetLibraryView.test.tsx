import { describe, test, expect, vi, beforeEach } from "vitest";
import type { ReactNode } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { toast } from "sonner";
import { act, fireEvent, render, screen, waitFor } from "./test-utils";
import { AssetLibraryView } from "../src/components/layout/AssetLibraryView";
import * as ipc from "../src/ipc";
import type { LibraryItem } from "../src/ipc/schemas";
import { useProjectStore } from "../src/store/project";
import { useUIStore } from "../src/store/uiStore";

const mockNavigate = vi.fn();

vi.mock("sonner", () => ({
  toast: {
    error: vi.fn(),
    success: vi.fn(),
    info: vi.fn(),
  },
}));

vi.mock("@tanstack/react-router", () => ({
  Link: ({ to, children, ...props }: { to: string; children: ReactNode }) => (
    <a href={to} {...props}>
      {children}
    </a>
  ),
  useNavigate: () => mockNavigate,
}));

const mockItems: LibraryItem[] = [
  {
    id: "lib-alpha",
    name: "alpha_photo.png",
    tags: ["nature"],
    media_type: "image/png",
    size: 2048,
    hash: "hash-alpha",
    added_at: "2026-10-30T10:00:00Z",
    removed: false,
    source: {},
  },
  {
    id: "lib-beta",
    name: "beta_notes.txt",
    tags: ["notes"],
    media_type: "text/plain",
    size: 1024,
    hash: "hash-beta",
    added_at: "2026-10-30T11:00:00Z",
    removed: false,
    source: {},
  },
];

describe("AssetLibraryView", () => {
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
    useProjectStore.setState({ activeProjectId: "proj-1" });
    useUIStore.setState({ assetLibraryViewMode: "list" });
  });

  function renderView() {
    return render(
      <QueryClientProvider client={queryClient}>
        <AssetLibraryView />
      </QueryClientProvider>,
    );
  }

  test("renders items from library.list", async () => {
    vi.spyOn(ipc, "opInvoke").mockImplementation(async (_projectId, name) => {
      if (name === "library.list") {
        return { step: {}, output: { items: mockItems } };
      }
      return { step: {}, output: {} };
    });

    renderView();

    await waitFor(() => {
      expect(screen.getByText("alpha_photo.png")).toBeInTheDocument();
      expect(screen.getByText("beta_notes.txt")).toBeInTheDocument();
    });
  });

  test("filters items by search input", async () => {
    vi.spyOn(ipc, "opInvoke").mockImplementation(async (_projectId, name) => {
      if (name === "library.list") {
        return { step: {}, output: { items: mockItems } };
      }
      return { step: {}, output: {} };
    });

    renderView();

    await waitFor(() => {
      expect(screen.getByText("alpha_photo.png")).toBeInTheDocument();
    });

    const searchInput = screen.getByPlaceholderText(/Search/i);
    fireEvent.change(searchInput, { target: { value: "beta" } });

    await waitFor(() => {
      expect(screen.queryByText("alpha_photo.png")).not.toBeInTheDocument();
      expect(screen.getByText("beta_notes.txt")).toBeInTheDocument();
    });
  });

  test("toggles between list and grid views", async () => {
    vi.spyOn(ipc, "opInvoke").mockImplementation(async (_projectId, name) => {
      if (name === "library.list") {
        return { step: {}, output: { items: mockItems } };
      }
      return { step: {}, output: {} };
    });

    renderView();

    await waitFor(() => {
      expect(screen.getByText("alpha_photo.png")).toBeInTheDocument();
    });

    expect(useUIStore.getState().assetLibraryViewMode).toBe("list");

    const gridBtn = screen.getByTestId("view-grid-button");
    fireEvent.click(gridBtn);

    expect(useUIStore.getState().assetLibraryViewMode).toBe("grid");
  });

  test("deletes an item and offers undo in toast", async () => {
    const invokedOps: string[] = [];
    vi.spyOn(ipc, "opInvoke").mockImplementation(async (_projectId, name) => {
      invokedOps.push(name);
      if (name === "library.list") {
        return { step: {}, output: { items: mockItems } };
      }
      if (name === "library.remove") {
        return { step: {}, output: { version: 2 } };
      }
      if (name === "library.undo") {
        return { step: {}, output: { applied: true, version: 1 } };
      }
      return { step: {}, output: {} };
    });

    renderView();

    await waitFor(() => {
      expect(screen.getByText("alpha_photo.png")).toBeInTheDocument();
    });

    // Select alpha_photo via checkbox
    const checkbox = screen.getByLabelText(/Select alpha_photo/i);
    fireEvent.click(checkbox);

    // Click remove in the selection bar
    const removeBtn = screen.getByTestId("remove-selected-button");
    await act(async () => {
      fireEvent.click(removeBtn);
    });

    await waitFor(() => {
      expect(invokedOps).toContain("library.remove");
      expect(toast.success).toHaveBeenCalled();
    });

    // Check toast action trigger
    const toastCall = vi.mocked(toast.success).mock.calls[0];
    const toastOptions = toastCall?.[1] as { action?: { onClick: () => void } } | undefined;
    expect(toastOptions?.action).toBeDefined();

    await act(async () => {
      toastOptions?.action?.onClick();
    });

    await waitFor(() => {
      expect(invokedOps).toContain("library.undo");
    });
  });

  test("handles Cmd+Z shortcut to trigger undo", async () => {
    const invokedOps: string[] = [];
    vi.spyOn(ipc, "opInvoke").mockImplementation(async (_projectId, name) => {
      invokedOps.push(name);
      if (name === "library.list") {
        return { step: {}, output: { items: mockItems } };
      }
      if (name === "library.undo") {
        return { step: {}, output: { applied: true, version: 1 } };
      }
      return { step: {}, output: {} };
    });

    const { container } = renderView();

    await waitFor(() => {
      expect(screen.getByText("alpha_photo.png")).toBeInTheDocument();
    });

    // Trigger Cmd+Z on the view container
    fireEvent.keyDown(container, { key: "z", metaKey: true });

    await waitFor(() => {
      expect(invokedOps).toContain("library.undo");
    });
  });
});
