import { useEffect } from "react";
import { useUIStore } from "@/store/uiStore";

/**
 * Bind Cmd+, (Ctrl+, off macOS) to open the settings dialog. The shell mounts
 * this once; opening leaves the route, board viewport and selection alone.
 * While another dialog is open the key is ignored, so settings never stacks
 * on top of it.
 */
export function useSettingsShortcut() {
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.defaultPrevented || e.repeat || e.altKey || e.shiftKey) return;
      if (!(e.metaKey || e.ctrlKey) || e.key !== ",") return;
      if (
        !useUIStore.getState().settingsOpen &&
        document.querySelector('[role="dialog"][data-state="open"]')
      ) {
        return;
      }
      e.preventDefault();
      useUIStore.getState().openSettings();
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, []);
}
