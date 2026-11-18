import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "./test-utils";
import { CollectionView } from "../src/components/layout/CollectionView";
import * as ipc from "../src/ipc";
import { mockOpList, mockOpPreview } from "../src/ipc/mock";
import { PINNED_TEXT_TOOLS_STORAGE_KEY } from "../src/lib/textOperations";
import { useBoardStore } from "../src/store/board";
import { useProjectStore } from "../src/store/project";
import { useUIStore } from "../src/store/uiStore";

const mockNavigate = vi.fn();
let mockParams = { id: "col_focus" };

vi.mock("sonner", () => ({
  toast: {
    error: vi.fn(),
    success: vi.fn(),
  },
}));

vi.mock("@tanstack/react-router", () => ({
  Link: ({
    to,
    children,
    ...props
  }: {
    to: string;
    children: React.ReactNode;
    [key: string]: unknown;
  }) => (
    <a href={to} {...props}>
      {children}
    </a>
  ),
  useNavigate: () => mockNavigate,
  useParams: () => mockParams,
  useRouter: () => ({ history: { canGoBack: () => false, back: vi.fn() } }),
}));

describe("CollectionView focus toolbar, recipe panel, and undo shortcuts", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    window.localStorage.clear();
    useUIStore.setState({ settingsOpen: false, commandPaletteOpen: false });
    mockParams = { id: "col_focus" };
    useProjectStore.setState({ activeProjectId: "default" });
    useBoardStore.getState().hydrate({
      id: "board-default",
      projectId: "default",
      version: 1,
      items: [],
    });
    vi.spyOn(ipc, "opList").mockImplementation(() => mockOpList({ projectId: "default" }));
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  test("focus toolbar appears only for text slots, pins/unpins tools to localStorage, and excludes split/join/inspect", async () => {
    vi.spyOn(ipc, "readAssetPreview").mockImplementation(async (_projectId, assetId) => {
      if (assetId === "ast_img") {
        return { url: "img-url", mediaType: "image/png", text: null };
      }
      return { url: "txt-url", mediaType: "text/markdown", text: "Hello  world" };
    });

    vi.spyOn(ipc, "opInvoke").mockImplementation(async (_projectId, name, input) => {
      if (name === "collection.get") {
        return {
          step: {} as never,
          output: {
            id: "col_focus",
            name: "Focus Collection",
            version: 1,
            slots: [
              { id: "s1", asset_id: "ast_txt" },
              { id: "s2", asset_id: "ast_img" },
            ],
          },
        };
      }
      if (name === "recipe.get") {
        const args = input as { slot_id: string };
        return {
          step: {} as never,
          output: {
            collection_id: "col_focus",
            slot_id: args.slot_id,
            root_asset: { id: "ast_txt", media_type: "text/markdown", size: 12 },
            current_asset: { id: "ast_txt", media_type: "text/markdown", size: 12 },
            stale: false,
            steps: [],
          },
        };
      }
      return { step: {} as never, output: {} };
    });

    render(<CollectionView />);
    await waitFor(() => {
      expect(screen.getByText("Focus Collection")).toBeInTheDocument();
      expect(screen.getByText("Text")).toBeInTheDocument();
      expect(screen.getByText("Image")).toBeInTheDocument();
    });

    // No focus toolbar or recipe panel before focusing a slot
    expect(screen.queryByRole("toolbar", { name: "Focused slot tools" })).toBeNull();
    expect(screen.queryByRole("complementary", { name: /Recipe/ })).toBeNull();

    // Focus text slot s1
    fireEvent.click(screen.getByTestId("slot-card-s1"));
    const toolbar = await screen.findByRole("toolbar", { name: "Focused slot tools" });
    expect(within(toolbar).getByRole("button", { name: "Replace text" })).toBeInTheDocument();
    expect(within(toolbar).queryByRole("button", { name: "Normalize text" })).toBeNull();
    expect(screen.getByRole("complementary", { name: "Recipe s1" })).toBeInTheDocument();

    // Open pin menu via + button
    fireEvent.click(within(toolbar).getByRole("button", { name: "Pin text tools" }));
    const menu = screen.getByRole("menu", { name: "Available text tools" });
    expect(
      within(menu).getByRole("menuitemcheckbox", { name: "Replace text" }),
    ).toBeInTheDocument();
    expect(
      within(menu).getByRole("menuitemcheckbox", { name: "Normalize text" }),
    ).toBeInTheDocument();
    expect(
      within(menu).getByRole("menuitemcheckbox", { name: "Apply template" }),
    ).toBeInTheDocument();
    expect(
      within(menu).queryByRole("menuitemcheckbox", { name: /Inspect|split|join/i }),
    ).toBeNull();

    // Pin Normalize text and Apply template
    fireEvent.click(within(menu).getByRole("menuitemcheckbox", { name: "Normalize text" }));
    fireEvent.click(within(menu).getByRole("menuitemcheckbox", { name: "Apply template" }));
    expect(within(toolbar).getByRole("button", { name: "Normalize text" })).toBeInTheDocument();
    expect(within(toolbar).getByRole("button", { name: "Apply template" })).toBeInTheDocument();
    expect(JSON.parse(window.localStorage.getItem(PINNED_TEXT_TOOLS_STORAGE_KEY) ?? "[]")).toEqual([
      "text.replace",
      "text.normalize",
      "text.template",
    ]);

    // Unpin Normalize text
    fireEvent.click(within(menu).getByRole("menuitemcheckbox", { name: "Normalize text" }));
    expect(within(toolbar).queryByRole("button", { name: "Normalize text" })).toBeNull();

    // First Escape closes the + menu while keeping s1 focused
    fireEvent.keyDown(window, { key: "Escape" });
    expect(screen.queryByRole("menu", { name: "Available text tools" })).toBeNull();
    expect(screen.getByRole("toolbar", { name: "Focused slot tools" })).toBeInTheDocument();
    expect(screen.getByRole("complementary", { name: "Recipe s1" })).toBeInTheDocument();

    // Focus image slot s2 -> toolbar disappears, recipe panel stays for s2
    fireEvent.click(screen.getByTestId("slot-card-s2"));
    expect(screen.queryByRole("toolbar", { name: "Focused slot tools" })).toBeNull();
    expect(await screen.findByRole("complementary", { name: "Recipe s2" })).toBeInTheDocument();

    // Escape clears focus
    fireEvent.keyDown(window, { key: "Escape" });
    expect(screen.queryByRole("complementary", { name: /Recipe/ })).toBeNull();
  });

  test("applies text.normalize and text.template in append mode with live preview diff", async () => {
    window.localStorage.setItem(
      PINNED_TEXT_TOOLS_STORAGE_KEY,
      JSON.stringify(["text.normalize", "text.template"]),
    );

    vi.spyOn(ipc, "readAssetPreview").mockResolvedValue({
      url: "txt-url",
      mediaType: "text/markdown",
      text: "Hello   world，test",
    });
    vi.spyOn(ipc, "readAssetText").mockResolvedValue("Hello   world，test");
    vi.spyOn(ipc, "opPreview").mockImplementation((projectId, name, input, previewKey) => {
      const payload =
        input && typeof input === "object"
          ? { ...(input as Record<string, unknown>), text: "Hello   world，test" }
          : { text: "Hello   world，test" };
      return mockOpPreview({ projectId, name, input: payload, previewKey });
    });

    let currentAsset = "ast_demo_style_guide";
    const opInvokeSpy = vi
      .spyOn(ipc, "opInvoke")
      .mockImplementation(async (_projectId, name, input) => {
        if (name === "collection.get") {
          return {
            step: {} as never,
            output: {
              id: "col_focus",
              name: "Focus Collection",
              version: 1,
              slots: [{ id: "s1", asset_id: currentAsset }],
            },
          };
        }
        if (name === "recipe.get") {
          return {
            step: {} as never,
            output: {
              collection_id: "col_focus",
              slot_id: "s1",
              root_asset: { id: "ast_demo_style_guide", media_type: "text/markdown", size: 20 },
              current_asset: { id: currentAsset, media_type: "text/markdown", size: 20 },
              stale: false,
              steps: [],
            },
          };
        }
        if (name === "text.normalize") {
          return { step: {} as never, output: { doc: "ast_norm_1" } };
        }
        if (name === "text.template") {
          return { step: {} as never, output: { doc: "ast_tpl_1" } };
        }
        if (name === "collection.set_slot") {
          currentAsset = (input as { asset: string }).asset;
          return { step: {} as never, output: { version: 2 } };
        }
        return { step: {} as never, output: {} };
      });

    render(<CollectionView />);
    await waitFor(() => {
      expect(screen.getByText("Text")).toBeInTheDocument();
    });
    fireEvent.click(screen.getByTestId("slot-card-s1"));

    // 1. Open text.normalize
    const normalizeBtn = await screen.findByRole("button", { name: "Normalize text" });
    fireEvent.click(normalizeBtn);
    await waitFor(() => {
      expect(screen.getByRole("dialog")).toBeInTheDocument();
      expect(screen.getByLabelText("Punctuation")).toBeInTheDocument();
    });

    fireEvent.change(screen.getByLabelText("Punctuation"), { target: { value: "halfwidth" } });
    await waitFor(() => {
      expect(screen.getByTestId("text-tool-diff")).toHaveTextContent("Hello world,test");
    });

    fireEvent.click(screen.getByRole("button", { name: /^Apply$/i }));
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).toBeNull();
    });
    expect(opInvokeSpy).toHaveBeenCalledWith("default", "text.normalize", {
      doc: "ast_demo_style_guide",
      whitespace: true,
      newline: true,
      punctuation: "halfwidth",
      unicode: "NFC",
    });
    expect(opInvokeSpy).toHaveBeenCalledWith("default", "collection.set_slot", {
      collection_id: "col_focus",
      slot_id: "s1",
      asset: "ast_norm_1",
    });

    // 2. Open text.template
    const templateBtn = await screen.findByRole("button", { name: "Apply template" });
    fireEvent.click(templateBtn);
    await waitFor(() => {
      expect(screen.getByRole("dialog")).toBeInTheDocument();
      expect(screen.getByLabelText("Template")).toBeInTheDocument();
    });

    fireEvent.change(screen.getByLabelText("Template"), {
      target: { value: "Title: {{title}}\n{{body}}" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Add variable" }));
    fireEvent.change(screen.getByPlaceholderText("Key"), { target: { value: "title" } });
    fireEvent.change(screen.getByPlaceholderText("Value"), { target: { value: "My Doc" } });

    await waitFor(() => {
      expect(screen.getByTestId("text-tool-diff")).toHaveTextContent("Title: My Doc");
    });

    fireEvent.click(screen.getByRole("button", { name: /^Apply$/i }));
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).toBeNull();
    });
    expect(opInvokeSpy).toHaveBeenCalledWith("default", "text.template", {
      doc: "ast_norm_1",
      template: "Title: {{title}}\n{{body}}",
      values: { title: "My Doc" },
    });
  });

  test("renders recipe panel with Agent badge, inspect summary, stale badge, and edits step via recipe.edit", async () => {
    vi.spyOn(ipc, "readAssetPreview").mockResolvedValue({
      url: "txt-url",
      mediaType: "text/markdown",
      text: "Draft content",
    });
    vi.spyOn(ipc, "readAssetText").mockResolvedValue("# Draft\n\nBody text");
    vi.spyOn(ipc, "opPreview").mockResolvedValue({
      text: "Header: Auto Title\n# Draft\n\nBody text",
    });

    let version = 3;
    const opInvokeSpy = vi.spyOn(ipc, "opInvoke").mockImplementation(async (_projectId, name) => {
      if (name === "collection.get") {
        return {
          step: {} as never,
          output: {
            id: "col_focus",
            name: "Focus Collection",
            version,
            slots: [{ id: "s1", asset_id: "ast_step_2", stale: true }],
          },
        };
      }
      if (name === "recipe.get") {
        return {
          step: {} as never,
          output: {
            collection_id: "col_focus",
            slot_id: "s1",
            root_asset: { id: "ast_root", media_type: "text/markdown", size: 512 },
            current_asset: { id: "ast_step_2", media_type: "text/markdown", size: 640 },
            stale: true,
            steps: [
              {
                step_id: "step_inspect",
                op: "text.inspect",
                op_version: "1.0.0",
                input: { doc: "ast_root" },
                input_sources: {},
                output: { title: "Draft Title", stats: { words: 42, lines: 5 } },
                actor: { kind: "Agent", id: "agent-1", label: null },
                read_only: true,
                stale: false,
              },
              {
                step_id: "step_tpl",
                op: "text.template",
                op_version: "1.0.0",
                input: {
                  doc: "ast_root",
                  template: "Header: {{title}}\n{{body}}",
                  values: { title: "Auto Title" },
                },
                input_sources: {
                  "/values/title": { step_id: "step_inspect", output_pointer: "/title" },
                },
                output: { doc: "ast_step_2" },
                actor: { kind: "Agent", id: "agent-1", label: null },
                read_only: false,
                stale: true,
              },
            ],
          },
        };
      }
      if (name === "recipe.edit") {
        version += 1;
        return {
          step: {} as never,
          output: {
            version,
            asset_id: "ast_step_2",
            stale: false,
            replayed_steps: 0,
            stale_steps: 0,
          },
        };
      }
      return { step: {} as never, output: {} };
    });

    render(<CollectionView />);
    await waitFor(() => {
      expect(screen.getByText("Focus Collection")).toBeInTheDocument();
    });

    // Slot card shows Stale badge
    expect(screen.getByTestId("slot-card-s1")).toHaveTextContent("Stale");

    // Focus slot
    fireEvent.click(screen.getByTestId("slot-card-s1"));
    const panel = await screen.findByRole("complementary", { name: "Recipe s1" });
    expect(within(panel).getByText("ast_root")).toBeInTheDocument();
    expect(within(panel).getByText("ast_step_2")).toBeInTheDocument();
    expect(within(panel).getByText(/Upstream changed/)).toBeInTheDocument();
    expect(within(panel).getByText("Title: Draft Title")).toBeInTheDocument();
    expect(within(panel).getByText("42 words · 5 lines")).toBeInTheDocument();
    expect(within(panel).getAllByText("Agent").length).toBeGreaterThanOrEqual(2);

    // Click editable step_tpl -> opens dialog in edit mode with prefilled values and upstream badge
    fireEvent.click(screen.getByTestId("recipe-step-step_tpl"));
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByText("From upstream step")).toBeInTheDocument();
    expect(within(dialog).getByLabelText("Template")).toHaveValue("Header: {{title}}\n{{body}}");

    await waitFor(() => {
      expect(screen.getByTestId("text-tool-diff")).toHaveTextContent("Header: Auto Title");
    });

    // Submit without touching upstream-bound `title` -> preserves original value
    fireEvent.click(within(dialog).getByRole("button", { name: /^Apply$/i }));
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).toBeNull();
    });

    expect(opInvokeSpy).toHaveBeenCalledWith("default", "recipe.edit", {
      collection_id: "col_focus",
      slot_id: "s1",
      step_id: "step_tpl",
      input: {
        doc: "ast_root",
        template: "Header: {{title}}\n{{body}}",
        values: { title: "Auto Title" },
      },
    });
  });

  test("Cmd+Z and Cmd+Shift+Z trigger collection.undo and collection.redo unless modal or input is focused", async () => {
    vi.spyOn(ipc, "readAssetPreview").mockResolvedValue({
      url: "txt-url",
      mediaType: "text/markdown",
      text: "Hello",
    });
    vi.spyOn(ipc, "readAssetText").mockResolvedValue("Hello");

    const opInvokeSpy = vi.spyOn(ipc, "opInvoke").mockImplementation(async (_projectId, name) => {
      if (name === "collection.get") {
        return {
          step: {} as never,
          output: {
            id: "col_focus",
            name: "Focus Collection",
            version: 1,
            slots: [{ id: "s1", asset_id: "ast_1" }],
          },
        };
      }
      if (name === "recipe.get") {
        return {
          step: {} as never,
          output: {
            collection_id: "col_focus",
            slot_id: "s1",
            root_asset: { id: "ast_1", media_type: "text/markdown", size: 5 },
            current_asset: { id: "ast_1", media_type: "text/markdown", size: 5 },
            stale: false,
            steps: [],
          },
        };
      }
      return { step: {} as never, output: { applied: true, version: 2 } };
    });

    render(<CollectionView />);
    await waitFor(() => {
      expect(screen.getByText("Focus Collection")).toBeInTheDocument();
    });

    // Cmd+Z triggers collection.undo
    await act(async () => {
      fireEvent.keyDown(window, { key: "z", metaKey: true });
    });
    expect(opInvokeSpy).toHaveBeenCalledWith("default", "collection.undo", {
      collection_id: "col_focus",
    });

    // Cmd+Shift+Z triggers collection.redo
    await act(async () => {
      fireEvent.keyDown(window, { key: "z", metaKey: true, shiftKey: true });
    });
    expect(opInvokeSpy).toHaveBeenCalledWith("default", "collection.redo", {
      collection_id: "col_focus",
    });

    opInvokeSpy.mockClear();

    // When a shell modal is open, Cmd+Z does NOT trigger collection.undo
    useUIStore.setState({ settingsOpen: true });
    await act(async () => {
      fireEvent.keyDown(window, { key: "z", metaKey: true });
    });
    expect(opInvokeSpy).not.toHaveBeenCalledWith("default", "collection.undo", expect.anything());
    useUIStore.setState({ settingsOpen: false });

    // When an input is focused inside the tool dialog, Cmd+Z does NOT trigger collection.undo
    await waitFor(() => {
      expect(screen.getByText("Text")).toBeInTheDocument();
    });
    fireEvent.click(screen.getByTestId("slot-card-s1"));
    const replaceBtn = await screen.findByRole("button", { name: "Replace text" });
    fireEvent.click(replaceBtn);
    const findInput = await screen.findByLabelText("Find");
    fireEvent.keyDown(findInput, { key: "z", metaKey: true });
    expect(opInvokeSpy).not.toHaveBeenCalledWith("default", "collection.undo", expect.anything());
  });
});
