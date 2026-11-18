import { useSettingsStore } from "@/store/settingsStore";
import { useUIStore } from "@/store/uiStore";
import { canvasAreaLayout, placementCenter } from "@/lib/canvasLayout";
import { centeredPosition } from "@/lib/library";

const EMPTY_VIEWPORT = { x: 0, y: 0, zoom: 1 } as const;
export const COLLECTION_CARD_SIZE = { width: 300, height: 200 } as const;

/** Flow center coordinate for placing a card at the visible canvas center or explicit drop point. */
export function calculatePlacementCenter(
  targetProjectId: string,
  dropPoint?: { x: number; y: number },
): { x: number; y: number } {
  if (dropPoint) {
    return dropPoint;
  }
  const viewport = useUIStore.getState().canvasViewports[targetProjectId] ?? EMPTY_VIEWPORT;
  const area = canvasAreaLayout({
    windowWidth: window.innerWidth,
    windowHeight: window.innerHeight,
    sidebarCollapsed: useSettingsStore.getState().sidebarCollapsed,
    panelOpen: useUIStore.getState().sessionPanelOpen,
  });
  return placementCenter(viewport, area);
}

/** Calculates top-left position for a card centered at viewport visible center or drop point. */
export function calculatePlacementPosition(
  targetProjectId: string,
  cardSize: { width: number; height: number },
  dropPoint?: { x: number; y: number },
): { x: number; y: number } {
  return centeredPosition(calculatePlacementCenter(targetProjectId, dropPoint), cardSize);
}
