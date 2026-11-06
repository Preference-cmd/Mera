import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, fireEvent, render, screen, waitFor, within } from "./test-utils";
import { SettingsDialog } from "../src/components/settings/SettingsDialog";
import { useSettingsShortcut } from "../src/hooks/useSettingsShortcut";
import { useProjectStore } from "../src/store/project";
import { useUIStore } from "../src/store/uiStore";

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

  test("the shortcuts section lists the real bindings read-only", () => {
    renderDialog();
    openAt("shortcuts");
    const dialog = screen.getByRole("dialog");
    expect(within(dialog).getByText("Open command palette")).toBeInTheDocument();
    expect(within(dialog).getByText("Open settings")).toBeInTheDocument();
    expect(within(dialog).queryByRole("combobox")).not.toBeInTheDocument();
  });
});
