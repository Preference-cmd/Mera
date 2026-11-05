import { useBoardStore } from "@/store/board";
import { useSettingsStore } from "@/store/settingsStore";
import { useUIStore } from "@/store/uiStore";
import { buildAddItemCommand, buildMoveItemCommand, createBoardBatch } from "@/board/batch";
import { canvasAreaLayout, placementCenter } from "@/lib/canvasLayout";
import { centeredPosition } from "@/lib/library";

const EMPTY_VIEWPORT = { x: 0, y: 0, zoom: 1 } as const;
export const COLLECTION_CARD_SIZE = { width: 300, height: 200 } as const;

/** Calculates top-left position for a card centered at viewport visible center or drop point. */
export function calculatePlacementPosition(
  targetProjectId: string,
  cardSize: { width: number; height: number },
  dropPoint?: { x: number; y: number },
): { x: number; y: number } {
  if (dropPoint) {
    return centeredPosition(dropPoint, cardSize);
  }
  const viewport = useUIStore.getState().canvasViewports[targetProjectId] ?? EMPTY_VIEWPORT;
  const area = canvasAreaLayout({
    windowWidth: window.innerWidth,
    windowHeight: window.innerHeight,
    sidebarCollapsed: useSettingsStore.getState().sidebarCollapsed,
    panelOpen: useUIStore.getState().sessionPanelOpen,
  });
  return centeredPosition(placementCenter(viewport, area), cardSize);
}

/** Places or moves a collection item on the target project's board at viewport center or drop point. */
export async function positionCollectionOnBoard(
  targetProjectId: string,
  collectionId: string,
  boardItemId?: string,
  dropPoint?: { x: number; y: number },
): Promise<void> {
  const size = COLLECTION_CARD_SIZE;
  const targetPos = calculatePlacementPosition(targetProjectId, size, dropPoint);

  let board = useBoardStore.getState();
  if (board.id === "" || board.projectId !== targetProjectId) {
    await board.load(targetProjectId);
    board = useBoardStore.getState();
  }

  const existing = board.items.find(
    (item) =>
      (boardItemId && item.id === boardItemId) ||
      (item.kind.type === "collection" && item.kind.collection_id === collectionId),
  );

  if (existing) {
    const command = buildMoveItemCommand(existing.id, targetPos);
    const batch = createBoardBatch(useBoardStore.getState().version, [command]);
    await useBoardStore.getState().apply(targetProjectId, batch);
  } else {
    const command = buildAddItemCommand({
      itemId: boardItemId ?? crypto.randomUUID(),
      kind: { type: "collection", collection_id: collectionId },
      position: targetPos,
      size,
    });
    const batch = createBoardBatch(useBoardStore.getState().version, [command]);
    await useBoardStore.getState().apply(targetProjectId, batch);
  }

  await useBoardStore
    .getState()
    .load(targetProjectId)
    .catch(() => false);
}
