import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import * as ipc from "../src/ipc";
import type { BoardCommand, CommandBatch } from "../src/board/batch";
import { positionCollectionOnBoard } from "../src/lib/boardPlacement";
import { useBoardStore } from "../src/store/board";
import { useProjectStore } from "../src/store/project";

const collectionCard = {
  id: "item-col-1",
  kind: { type: "collection" as const, collection_id: "col-1" },
  position: { x: 0, y: 0 },
  size: { width: 300, height: 200 },
  z: 1,
  locked: false,
};

describe("positionCollectionOnBoard", () => {
  beforeEach(() => {
    useProjectStore.setState({ activeProjectId: "p1" });
    // The cached board predates collection.create, so it lacks the new card.
    useBoardStore.setState({ id: "board-p1", projectId: "p1", version: 3, items: [] });
    vi.spyOn(ipc, "getBoardSnapshot").mockResolvedValue({
      id: "board-p1",
      projectId: "p1",
      version: 4,
      items: [collectionCard],
    });
  });

  afterEach(() => vi.restoreAllMocks());

  test("moves the card collection.create placed at the origin to the drop point", async () => {
    const applySpy = vi.spyOn(ipc, "applyBoardCommands").mockResolvedValue({
      status: "applied",
      version: 5,
      changes: [],
      diagnostics: [],
      historyEntryId: null,
    });

    await positionCollectionOnBoard("p1", "col-1", "item-col-1", { x: 500, y: 400 });

    const batch = applySpy.mock.calls[0]?.[1] as CommandBatch<BoardCommand>;
    expect(batch.base_version).toBe(4);
    expect(batch.commands).toEqual([
      { type: "move_item", item_id: "item-col-1", position: { x: 350, y: 300 } },
    ]);
  });

  test("reports a rejected placement instead of failing silently", async () => {
    vi.spyOn(ipc, "applyBoardCommands").mockResolvedValue({
      status: "rejected",
      version: 4,
      changes: [],
      diagnostics: [{ message: "version conflict" }],
      historyEntryId: null,
    });

    await expect(
      positionCollectionOnBoard("p1", "col-1", "item-col-1", { x: 500, y: 400 }),
    ).rejects.toThrow("version conflict");
  });
});
