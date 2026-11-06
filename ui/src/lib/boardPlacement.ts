import * as m from "$paraglide/messages";
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

  // collection.create has just appended its own card at the origin, so the
  // cached snapshot is stale: reload to find that card and the current version.
  await useBoardStore.getState().load(targetProjectId);
  const board = useBoardStore.getState();
  if (board.projectId !== targetProjectId) return;

  const existing = board.items.find(
    (item) =>
      (boardItemId && item.id === boardItemId) ||
      (item.kind.type === "collection" && item.kind.collection_id === collectionId),
  );

  const command = existing
    ? buildMoveItemCommand(existing.id, targetPos)
    : buildAddItemCommand({
        itemId: boardItemId ?? crypto.randomUUID(),
        kind: { type: "collection", collection_id: collectionId },
        position: targetPos,
        size,
      });
  const batch = createBoardBatch(board.version, [command]);
  const result = await useBoardStore.getState().apply(targetProjectId, batch);

  await useBoardStore
    .getState()
    .load(targetProjectId)
    .catch(() => false);
  if (result.status === "rejected") {
    const firstDiag = result.diagnostics?.[0] as { message?: string } | undefined;
    throw new Error(firstDiag?.message ?? m["library.placeFailed"]());
  }
}
