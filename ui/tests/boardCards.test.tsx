import { describe, test, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, act } from "./test-utils";
import {
  NoteCard,
  CollectionCard,
  AssetRefCard,
  WorkflowRefCard,
  RunRefCard,
  PlaceholderCard,
  getCardRenderer,
} from "../src/board/registry";
import { useProjectStore } from "../src/store/project";
import * as ipc from "../src/ipc";

const mockNavigate = vi.fn();
vi.mock("@tanstack/react-router", () => ({
  useNavigate: () => mockNavigate,
}));

vi.mock("$paraglide/messages", () => ({
  "collections.slot": () => "slot",
  "collections.slots": () => "slots",
  "collections.assetCountOne": () => "1 asset",
  "collections.assetCountMany": ({ count }: { count: number }) => `${count} assets`,
  "collections.loading": () => "Loading…",
  "board.note": () => "Note",
  "board.asset": () => "Asset",
  "board.workflow": () => "Workflow",
  "board.run": () => "Run",
  "board.unregisteredItem": () => "Unregistered board item",
  "board.doubleClickToEdit": () => "Double-click to edit note…",
  "board.notePlaceholder": () =>
    "Type note content… (Cmd/Ctrl+Enter or blur to save, Esc to cancel)",
  "board.locked": () => "Locked",
  "assets.typeLabel.image": () => "Image",
  "assets.typeLabel.text": () => "Text",
  "assets.typeLabel.video": () => "Video",
  "assets.typeLabel.audio": () => "Audio",
  "assets.typeLabel.other": () => "Other",
}));

describe("Board card renderers and registry", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useProjectStore.setState({ activeProjectId: "proj-cards" });
  });

  test("getCardRenderer returns component for known kinds and PlaceholderCard for unknown kind", () => {
    expect(getCardRenderer("note")).toBe(NoteCard);
    expect(getCardRenderer("collection")).toBe(CollectionCard);
    expect(getCardRenderer("asset_ref")).toBe(AssetRefCard);
    expect(getCardRenderer("workflow_ref")).toBe(WorkflowRefCard);
    expect(getCardRenderer("run_ref")).toBe(RunRefCard);
    expect(getCardRenderer("some_unregistered_kind")).toBe(PlaceholderCard);
  });

  test("PlaceholderCard renders type name and item id", () => {
    render(
      <PlaceholderCard
        id="item-mystery"
        kind={{ type: "custom_plugin_type", val: 123 }}
        size={{ width: 200, height: 100 }}
        locked={false}
      />,
    );
    expect(screen.getByText("custom_plugin_type")).toBeInTheDocument();
    expect(screen.getByText(/id: item-mystery/)).toBeInTheDocument();
  });

  test("AssetRefCard renders asset name and text preview without raw board item id", async () => {
    vi.spyOn(ipc, "opInvoke").mockResolvedValue({
      step: { id: "s1", op: "asset.list", outcome: "succeeded" },
      output: {
        assets: [
          {
            id: "artifact-xyz",
            name: "hero_prompt.txt",
            media_type: "text/plain",
            size: 24,
            origin: "step-1",
          },
        ],
      },
    });
    vi.spyOn(ipc, "readAssetPreview").mockResolvedValue({
      url: "asset-url",
      mediaType: "text/plain",
      text: "Warm golden hour lighting",
    });

    render(
      <AssetRefCard
        id="item-asset"
        kind={{ type: "asset_ref", artifact_id: "artifact-xyz" }}
        size={{ width: 240, height: 160 }}
        locked={false}
      />,
    );
    await screen.findByText("hero_prompt.txt");
    expect(screen.getByText("Warm golden hour lighting")).toBeInTheDocument();
    expect(screen.queryByText(/id: item-asset/)).not.toBeInTheDocument();
  });

  test("WorkflowRefCard renders Workflow label and resolved workflow name without raw board item id", async () => {
    vi.spyOn(ipc, "opInvoke").mockResolvedValue({
      step: { id: "s1", op: "dynamic_workflow.list", outcome: "succeeded" },
      output: {
        workflows: [{ id: "wf-main-pipeline", name: "Main Pipeline", version: 1, step_count: 2 }],
      },
    });

    render(
      <WorkflowRefCard
        id="item-wf"
        kind={{ type: "workflow_ref", workflow_id: "wf-main-pipeline" }}
        size={{ width: 200, height: 100 }}
        locked={false}
      />,
    );
    expect(screen.getByText("Workflow")).toBeInTheDocument();
    await screen.findByText("Main Pipeline");
    expect(screen.queryByText(/id: item-wf/)).not.toBeInTheDocument();
  });

  test("RunRefCard selects on single click and opens the library run filter on double click", () => {
    render(
      <RunRefCard
        id="item-run"
        kind={{ type: "run_ref", run_id: "run-alpha-42" }}
        size={{ width: 200, height: 100 }}
        locked={false}
      />,
    );
    expect(screen.getByText("Run")).toBeInTheDocument();
    expect(screen.getByText("run-alpha-42")).toBeInTheDocument();

    fireEvent.click(screen.getByTestId("run-ref-card"));
    expect(mockNavigate).not.toHaveBeenCalled();

    fireEvent.doubleClick(screen.getByTestId("run-ref-card"));
    expect(mockNavigate).toHaveBeenCalledWith({ to: "/library", search: { type: "run" } });
  });

  test("NoteCard renders text, double-click triggers onStartEdit, blur or Cmd+Enter submits update", () => {
    const onStartEdit = vi.fn();
    const onEndEdit = vi.fn();
    const onUpdateNote = vi.fn();

    const { rerender } = render(
      <NoteCard
        id="note-1"
        kind={{ type: "note", content: "Original note" }}
        size={{ width: 200, height: 100 }}
        locked={false}
        isEditing={false}
        onStartEdit={onStartEdit}
        onEndEdit={onEndEdit}
        onUpdateNote={onUpdateNote}
      />,
    );

    expect(screen.getByText("Original note")).toBeInTheDocument();

    // Double-click to start editing
    fireEvent.doubleClick(screen.getByTestId("note-card"));
    expect(onStartEdit).toHaveBeenCalled();

    // Re-render in edit mode
    rerender(
      <NoteCard
        id="note-1"
        kind={{ type: "note", content: "Original note" }}
        size={{ width: 200, height: 100 }}
        locked={false}
        isEditing={true}
        onStartEdit={onStartEdit}
        onEndEdit={onEndEdit}
        onUpdateNote={onUpdateNote}
      />,
    );

    const textarea = screen.getByTestId("note-textarea") as HTMLTextAreaElement;
    expect(textarea.value).toBe("Original note");

    // Change text
    fireEvent.change(textarea, { target: { value: "New edited content" } });

    // Submit with Cmd+Enter
    fireEvent.keyDown(textarea, { key: "Enter", metaKey: true });
    expect(onUpdateNote).toHaveBeenCalledWith("New edited content");
    expect(onEndEdit).toHaveBeenCalled();

    // If unchanged, blur sends nothing
    onUpdateNote.mockClear();
    onEndEdit.mockClear();

    rerender(
      <NoteCard
        id="note-1"
        kind={{ type: "note", content: "Unchanged text" }}
        size={{ width: 200, height: 100 }}
        locked={false}
        isEditing={true}
        onStartEdit={onStartEdit}
        onEndEdit={onEndEdit}
        onUpdateNote={onUpdateNote}
      />,
    );

    const textarea2 = screen.getByTestId("note-textarea") as HTMLTextAreaElement;
    fireEvent.blur(textarea2);
    expect(onUpdateNote).not.toHaveBeenCalled();
    expect(onEndEdit).toHaveBeenCalled();

    // Escape cancels edit without calling onUpdateNote
    onUpdateNote.mockClear();
    onEndEdit.mockClear();

    rerender(
      <NoteCard
        id="note-1"
        kind={{ type: "note", content: "Initial" }}
        size={{ width: 200, height: 100 }}
        locked={false}
        isEditing={true}
        onStartEdit={onStartEdit}
        onEndEdit={onEndEdit}
        onUpdateNote={onUpdateNote}
      />,
    );

    const textarea3 = screen.getByTestId("note-textarea") as HTMLTextAreaElement;
    fireEvent.change(textarea3, { target: { value: "Modified before escape" } });
    fireEvent.keyDown(textarea3, { key: "Escape" });
    expect(onUpdateNote).not.toHaveBeenCalled();
    expect(onEndEdit).toHaveBeenCalled();
  });

  test("CollectionCard displays name and asset count via collection.get, refreshes on collection.changed, navigates on double click", async () => {
    let eventCallback: ((evt: ipc.Event) => void) | null = null;
    vi.spyOn(ipc, "subscribeEvents").mockImplementation((cb) => {
      eventCallback = cb;
      return () => {
        eventCallback = null;
      };
    });

    const opInvokeSpy = vi.spyOn(ipc, "opInvoke").mockResolvedValue({
      step: { id: "step-1", op: "collection.get", outcome: "succeeded" },
      output: {
        id: "col-123",
        name: "My Collection",
        version: 1,
        slots: [
          { id: "slot-1", asset_id: "ast-1" },
          { id: "slot-2", asset_id: "ast-2" },
        ],
      },
    });
    vi.spyOn(ipc, "readAssetPreview").mockResolvedValue({
      url: "asset-url",
      mediaType: "text/plain",
      text: "Slot snippet",
    });

    render(
      <CollectionCard
        id="item-col"
        kind={{ type: "collection", collection_id: "col-123" }}
        size={{ width: 200, height: 100 }}
        locked={false}
      />,
    );

    expect(opInvokeSpy).toHaveBeenCalledWith("proj-cards", "collection.get", {
      collection_id: "col-123",
    });

    // Wait for collection document to load
    await screen.findByText("My Collection");
    expect(screen.getByText("2 assets")).toBeInTheDocument();
    expect(screen.queryByText("col-123")).not.toBeInTheDocument();

    // Single click selects without leaving canvas; double-click navigates to /collections/$id
    fireEvent.click(screen.getByTestId("collection-card"));
    expect(mockNavigate).not.toHaveBeenCalled();

    fireEvent.doubleClick(screen.getByTestId("collection-card"));
    expect(mockNavigate).toHaveBeenCalledWith({
      to: "/collections/$id",
      params: { id: "col-123" },
    });

    // Trigger collection.changed event
    opInvokeSpy.mockResolvedValueOnce({
      step: { id: "step-2", op: "collection.get", outcome: "succeeded" },
      output: {
        id: "col-123",
        name: "Updated Collection Name",
        version: 2,
        slots: [{ id: "slot-1", asset_id: "ast-1" }],
      },
    });

    act(() => {
      eventCallback?.({
        type: "document",
        kind: "collection.changed",
        projectId: "proj-cards",
        documentId: "col-123",
        version: 2,
      });
    });

    await screen.findByText("Updated Collection Name");
    expect(screen.getByText("1 asset")).toBeInTheDocument();
  });
  test("Cmd+Enter followed by blur sends exactly one update_note", () => {
    const onStartEdit = vi.fn();
    const onEndEdit = vi.fn();
    const onUpdateNote = vi.fn();

    render(
      <NoteCard
        id="note-double-submit"
        kind={{ type: "note", content: "Original" }}
        size={{ width: 200, height: 100 }}
        locked={false}
        isEditing={true}
        onStartEdit={onStartEdit}
        onEndEdit={onEndEdit}
        onUpdateNote={onUpdateNote}
      />,
    );

    const textarea = screen.getByTestId("note-textarea") as HTMLTextAreaElement;
    fireEvent.change(textarea, { target: { value: "New content" } });

    // 1. Submit with Cmd+Enter
    fireEvent.keyDown(textarea, { key: "Enter", metaKey: true });
    expect(onUpdateNote).toHaveBeenCalledTimes(1);
    expect(onUpdateNote).toHaveBeenCalledWith("New content");
    expect(onEndEdit).toHaveBeenCalledTimes(1);

    // 2. Immediately blur after Cmd+Enter (e.g. browser focus change)
    fireEvent.blur(textarea);

    // Must NOT call onUpdateNote or onEndEdit a second time
    expect(onUpdateNote).toHaveBeenCalledTimes(1);
    expect(onEndEdit).toHaveBeenCalledTimes(1);
  });

  test("Esc followed by blur sends no update_note", () => {
    const onStartEdit = vi.fn();
    const onEndEdit = vi.fn();
    const onUpdateNote = vi.fn();

    render(
      <NoteCard
        id="note-esc-blur"
        kind={{ type: "note", content: "Original" }}
        size={{ width: 200, height: 100 }}
        locked={false}
        isEditing={true}
        onStartEdit={onStartEdit}
        onEndEdit={onEndEdit}
        onUpdateNote={onUpdateNote}
      />,
    );

    const textarea = screen.getByTestId("note-textarea") as HTMLTextAreaElement;
    fireEvent.change(textarea, { target: { value: "Changed before esc" } });

    // 1. Cancel with Escape
    fireEvent.keyDown(textarea, { key: "Escape" });
    expect(onUpdateNote).not.toHaveBeenCalled();
    expect(onEndEdit).toHaveBeenCalledTimes(1);

    // 2. Followed by blur
    fireEvent.blur(textarea);

    // Still must not call onUpdateNote
    expect(onUpdateNote).not.toHaveBeenCalled();
    expect(onEndEdit).toHaveBeenCalledTimes(1);
  });
  test("locked NoteCard does not enter edit mode on double-click and does not render textarea when isEditing is true", () => {
    const onStartEdit = vi.fn();
    const onEndEdit = vi.fn();
    const onUpdateNote = vi.fn();

    const { rerender } = render(
      <NoteCard
        id="note-locked"
        kind={{ type: "note", content: "Locked Note Content" }}
        size={{ width: 200, height: 100 }}
        locked={true}
        isEditing={false}
        onStartEdit={onStartEdit}
        onEndEdit={onEndEdit}
        onUpdateNote={onUpdateNote}
      />,
    );

    // Double-click should NOT trigger onStartEdit
    fireEvent.doubleClick(screen.getByTestId("note-card"));
    expect(onStartEdit).not.toHaveBeenCalled();

    // Even if isEditing is true, locked card must not show textarea
    rerender(
      <NoteCard
        id="note-locked"
        kind={{ type: "note", content: "Locked Note Content" }}
        size={{ width: 200, height: 100 }}
        locked={true}
        isEditing={true}
        onStartEdit={onStartEdit}
        onEndEdit={onEndEdit}
        onUpdateNote={onUpdateNote}
      />,
    );

    expect(screen.queryByTestId("note-textarea")).not.toBeInTheDocument();
    expect(screen.getByText("Locked Note Content")).toBeInTheDocument();
  });

  test("locked card renders lock indicator with accessible label across all card types", () => {
    const { rerender } = render(
      <NoteCard
        id="note-lock"
        kind={{ type: "note", content: "Note" }}
        size={{ width: 200, height: 100 }}
        locked={true}
      />,
    );
    expect(screen.getByTestId("lock-indicator")).toHaveAttribute("aria-label", "Locked");

    rerender(
      <NoteCard
        id="note-lock"
        kind={{ type: "note", content: "Note" }}
        size={{ width: 200, height: 100 }}
        locked={false}
      />,
    );
    expect(screen.queryByTestId("lock-indicator")).not.toBeInTheDocument();

    // CollectionCard
    rerender(
      <CollectionCard
        id="col-lock"
        kind={{ type: "collection", collection_id: "col-1" }}
        size={{ width: 200, height: 100 }}
        locked={true}
      />,
    );
    expect(screen.getByTestId("lock-indicator")).toHaveAttribute("aria-label", "Locked");

    // AssetRefCard
    rerender(
      <AssetRefCard
        id="asset-lock"
        kind={{ type: "asset_ref", artifact_id: "art-1" }}
        size={{ width: 200, height: 100 }}
        locked={true}
      />,
    );
    expect(screen.getByTestId("lock-indicator")).toHaveAttribute("aria-label", "Locked");

    // WorkflowRefCard
    rerender(
      <WorkflowRefCard
        id="wf-lock"
        kind={{ type: "workflow_ref", workflow_id: "wf-1" }}
        size={{ width: 200, height: 100 }}
        locked={true}
      />,
    );
    expect(screen.getByTestId("lock-indicator")).toHaveAttribute("aria-label", "Locked");

    // RunRefCard
    rerender(
      <RunRefCard
        id="run-lock"
        kind={{ type: "run_ref", run_id: "run-1" }}
        size={{ width: 200, height: 100 }}
        locked={true}
      />,
    );
    expect(screen.getByTestId("lock-indicator")).toHaveAttribute("aria-label", "Locked");

    // PlaceholderCard
    rerender(
      <PlaceholderCard
        id="ph-lock"
        kind={{ type: "unknown_type" }}
        size={{ width: 200, height: 100 }}
        locked={true}
      />,
    );
    expect(screen.getByTestId("lock-indicator")).toHaveAttribute("aria-label", "Locked");
  });
});
