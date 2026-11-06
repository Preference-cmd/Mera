import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, fireEvent, render, screen, waitFor, within } from "./test-utils";
import { SettingsDialog } from "../src/components/settings/SettingsDialog";
import { useSettingsShortcut } from "../src/hooks/useSettingsShortcut";
import { useProjectStore } from "../src/store/project";
import { useUIStore } from "../src/store/uiStore";
import * as ipc from "../src/ipc";

vi.mock("sonner", () => ({
  toast: { error: vi.fn(), success: vi.fn(), info: vi.fn() },
}));

const PROJECT = {
  id: "default",
  name: "Default",
  description: "Default workspace project",
  createdAt: "2026-08-18T00:00:00Z",
  updatedAt: "2026-08-18T00:00:00Z",
};

function ShortcutMount() {
  useSettingsShortcut();
  return null;
}

function renderDialog() {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  return render(
    <QueryClientProvider client={queryClient}>
      <ShortcutMount />
      <SettingsDialog />
    </QueryClientProvider>,
  );
}

function openAt(...args: Parameters<ReturnType<typeof useUIStore.getState>["openSettings"]>) {
  act(() => {
    useUIStore.getState().openSettings(...args);
  });
}

describe("SettingsDialog", () => {
  beforeEach(() => {
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

  test("openSettings() shows the dialog without changing the route", () => {
    window.history.pushState({}, "", "/library");
    renderDialog();
    expect(screen.queryByTestId("settings-dialog")).not.toBeInTheDocument();

    openAt();

    const dialog = screen.getByRole("dialog");
    expect(dialog).toBeInTheDocument();
    expect(within(dialog).getByRole("heading", { name: "General" })).toBeInTheDocument();
    expect(window.location.pathname).toBe("/library");
    window.history.pushState({}, "", "/");
  });

  test("Cmd+, opens the dialog and Esc closes it", async () => {
    renderDialog();
    act(() => {
      window.dispatchEvent(
        new KeyboardEvent("keydown", { key: ",", metaKey: true, bubbles: true, cancelable: true }),
      );
    });
    expect(useUIStore.getState().settingsOpen).toBe(true);
    const dialog = screen.getByRole("dialog");

    fireEvent.keyDown(dialog, { key: "Escape" });

    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(useUIStore.getState().settingsOpen).toBe(false);
  });

  test("the close button closes the dialog", async () => {
    renderDialog();
    openAt("about");
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  });

  test("the workspace group is titled with the workspace and disabled when none is open", () => {
    renderDialog();
    openAt();
    expect(screen.getByRole("group", { name: "Workspace: Default" })).toBeInTheDocument();
    act(() => useUIStore.getState().closeSettings());

    useProjectStore.setState({ projects: [], activeProjectId: "default" });
    openAt("workspace-folders");

    const group = screen.getByRole("group", { name: "Workspace" });
    expect(group).toHaveAttribute("aria-disabled", "true");
    for (const button of within(group).getAllByRole("button")) {
      expect(button).toBeDisabled();
    }
    expect(screen.getAllByText("No workspace open").length).toBeGreaterThan(0);
  });

  test("the folders section lists the workspace folders from the mock", async () => {
    renderDialog();
    openAt("workspace-folders");
    await waitFor(() => {
      expect(screen.getAllByTestId("workspace-folder-row")).toHaveLength(1);
    });
    expect(
      screen.getByText("The agent can read and import files only inside these folders."),
    ).toBeInTheDocument();
  });

  test("renaming commits on Enter through project.rename and refreshes the list", async () => {
    const invoke = vi.spyOn(ipc, "opInvoke");
    renderDialog();
    openAt("workspace-general");

    const input = screen.getByRole("textbox", { name: "Name" });
    fireEvent.change(input, { target: { value: "  Spring  " } });
    fireEvent.keyDown(input, { key: "Enter" });

    await waitFor(() => {
      expect(useProjectStore.getState().projects.find((p) => p.id === "default")?.name).toBe(
        "Spring",
      );
    });
    expect(invoke).toHaveBeenCalledWith("default", "project.rename", { name: "Spring" });
    expect(screen.getByRole("group", { name: "Workspace: Spring" })).toBeInTheDocument();
  });

  test("an empty name is rejected inline and never sent", async () => {
    const invoke = vi.spyOn(ipc, "opInvoke");
    renderDialog();
    openAt("workspace-general");

    const input = screen.getByRole("textbox", { name: "Name" });
    fireEvent.change(input, { target: { value: "   " } });
    fireEvent.blur(input);

    expect(await screen.findByRole("alert")).toHaveTextContent("Name can't be empty");
    expect(input).toHaveAttribute("aria-invalid", "true");
    expect(invoke.mock.calls.some(([, name]) => name === "project.rename")).toBe(false);
  });

  test("Esc in the name field reverts the draft and keeps the dialog open", () => {
    renderDialog();
    openAt("workspace-general");

    const input = screen.getByRole("textbox", { name: "Name" });
    input.focus();
    fireEvent.change(input, { target: { value: "Draft" } });
    fireEvent.keyDown(input, { key: "Escape" });

    expect(input).toHaveValue("Default");
    expect(screen.getByRole("dialog")).toBeInTheDocument();
  });

  test("a workspace other than the active one is shown without switching", () => {
    useProjectStore.setState({
      projects: [PROJECT, { ...PROJECT, id: "other", name: "Other" }],
      activeProjectId: PROJECT.id,
    });
    renderDialog();
    openAt("workspace-general", "other");

    expect(screen.getByRole("group", { name: "Workspace: Other" })).toBeInTheDocument();
    expect(screen.getByRole("textbox", { name: "Name" })).toHaveValue("Other");
    expect(useProjectStore.getState().activeProjectId).toBe(PROJECT.id);

    act(() => useUIStore.getState().closeSettings());
    expect(useUIStore.getState().settingsWorkspaceId).toBeNull();
  });

  test("the shortcuts section lists the real bindings read-only", () => {
    renderDialog();
    openAt("shortcuts");
    const dialog = screen.getByRole("dialog");
    expect(within(dialog).getByText("Open command palette")).toBeInTheDocument();
    expect(within(dialog).getByText("Open settings")).toBeInTheDocument();
    expect(within(dialog).queryByRole("combobox")).not.toBeInTheDocument();
  });
});
