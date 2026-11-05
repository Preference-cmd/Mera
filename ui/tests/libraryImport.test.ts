import { beforeEach, describe, expect, it, vi } from "vitest";

const opInvoke = vi.fn();
vi.mock("@/ipc", () => ({ opInvoke: (...args: unknown[]) => opInvoke(...args) }));

const { PartialLibraryImportError, importLibraryItemsToProject } =
  await import("../src/lib/globalLibrary");
const { libraryImportErrorMessage } = await import("../src/hooks/useGlobalLibrary");

const item = {
  id: "lib-1",
  name: "sunset.png",
  tags: [],
  media_type: "image/png",
  size: 1,
  hash: "h1",
  added_at: "2026-11-05T00:00:00Z",
  removed: false,
};

describe("importLibraryItemsToProject", () => {
  beforeEach(() => opInvoke.mockReset());

  it("imports, then groups the new assets into a collection", async () => {
    opInvoke
      .mockResolvedValueOnce({ output: { assets: ["asset-9"] } })
      .mockResolvedValueOnce({ output: { collection_id: "col-1" } });

    await expect(importLibraryItemsToProject("p1", [item])).resolves.toEqual({
      collectionId: "col-1",
      boardItemId: undefined,
    });
    expect(opInvoke).toHaveBeenNthCalledWith(1, "p1", "asset.import", {
      library_items: ["lib-1"],
    });
    expect(opInvoke).toHaveBeenNthCalledWith(2, "p1", "collection.create", {
      name: "sunset.png",
      assets: ["asset-9"],
    });
  });

  it("reports a partial import when the collection step fails", async () => {
    opInvoke
      .mockResolvedValueOnce({ output: { assets: ["asset-9", "asset-10"] } })
      .mockRejectedValueOnce(new Error("boom"));

    const error = await importLibraryItemsToProject("p1", [item, { ...item, id: "lib-2" }]).catch(
      (e: unknown) => e,
    );
    expect(error).toBeInstanceOf(PartialLibraryImportError);
    expect((error as InstanceType<typeof PartialLibraryImportError>).importedCount).toBe(2);
    expect(libraryImportErrorMessage(error)).toContain("2");
  });

  it("passes a failed import through unchanged", async () => {
    opInvoke.mockRejectedValueOnce(new Error("not found"));
    const error = await importLibraryItemsToProject("p1", [item]).catch((e: unknown) => e);
    expect(error).not.toBeInstanceOf(PartialLibraryImportError);
    expect(libraryImportErrorMessage(error)).toBe("not found");
  });
});
