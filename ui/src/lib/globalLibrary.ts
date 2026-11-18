import { opInvoke } from "@/ipc";
import {
  LibraryItemListSchema,
  WorkspaceFolderListSchema,
  type LibraryItem,
  type WorkspaceFolder,
} from "@/ipc/schemas";
import * as m from "$paraglide/messages";

export type MediaTypeFilter = "all" | "image" | "text" | "video" | "audio" | "other";

export function getMediaTypeGroup(
  mediaType: string,
): "image" | "text" | "video" | "audio" | "other" {
  const lower = mediaType.toLowerCase();
  if (lower.startsWith("image/")) return "image";
  if (lower.startsWith("text/") || lower === "application/json") return "text";
  if (lower.startsWith("video/")) return "video";
  if (lower.startsWith("audio/")) return "audio";
  return "other";
}

/** Human-friendly localized label for an asset's media type (never raw MIME). */
export function friendlyMediaTypeLabel(mediaType: string): string {
  switch (getMediaTypeGroup(mediaType)) {
    case "image":
      return m["assets.typeLabel.image"]();
    case "text":
      return m["assets.typeLabel.text"]();
    case "video":
      return m["assets.typeLabel.video"]();
    case "audio":
      return m["assets.typeLabel.audio"]();
    case "other":
      return m["assets.typeLabel.other"]();
  }
}

export function filterLibraryItems(
  items: LibraryItem[],
  search: string,
  selectedTag: string | null,
  selectedMediaType: MediaTypeFilter,
): LibraryItem[] {
  const needle = search.trim().toLowerCase();
  return items.filter((item) => {
    if (needle && !item.name.toLowerCase().includes(needle)) {
      return false;
    }
    if (selectedTag && !item.tags.includes(selectedTag)) {
      return false;
    }
    if (selectedMediaType !== "all") {
      const group = getMediaTypeGroup(item.media_type);
      if (group !== selectedMediaType) return false;
    }
    return true;
  });
}

export function extractUniqueTags(items: LibraryItem[]): string[] {
  const tagSet = new Set<string>();
  for (const item of items) {
    for (const tag of item.tags) {
      if (tag.trim()) tagSet.add(tag.trim());
    }
  }
  return Array.from(tagSet).sort((a, b) => a.localeCompare(b));
}

export function formatLibraryImportName(items: LibraryItem[]): string {
  if (items.length === 1) {
    return items[0].name;
  }
  return m["assets.importMultipleName"]({ count: items.length });
}

/* ───── Typed IPC calls ───── */

export async function listGlobalLibrary(
  projectId: string,
  filters?: {
    name_contains?: string;
    tag?: string;
    media_type?: string;
    include_removed?: boolean;
  },
): Promise<LibraryItem[]> {
  const result = await opInvoke(projectId, "library.list", filters ?? {});
  return LibraryItemListSchema.parse(result.output).items;
}

export async function addGlobalLibraryFiles(
  projectId: string,
  paths: string[],
): Promise<{ items: Array<{ id: string; name: string; deduplicated: boolean }>; version: number }> {
  const result = await opInvoke(projectId, "library.add", { paths });
  return result.output as {
    items: Array<{ id: string; name: string; deduplicated: boolean }>;
    version: number;
  };
}

export async function renameGlobalLibraryItem(
  projectId: string,
  id: string,
  name: string,
): Promise<{ version: number }> {
  const result = await opInvoke(projectId, "library.rename", { id, name });
  return result.output as { version: number };
}

export async function tagGlobalLibraryItem(
  projectId: string,
  id: string,
  tags: string[],
): Promise<{ version: number }> {
  const result = await opInvoke(projectId, "library.tag", { id, tags });
  return result.output as { version: number };
}

export async function removeGlobalLibraryItems(
  projectId: string,
  ids: string[],
): Promise<{ version: number }> {
  const result = await opInvoke(projectId, "library.remove", { ids });
  return result.output as { version: number };
}

export async function undoGlobalLibrary(
  projectId: string,
): Promise<{ applied: boolean; version: number }> {
  const result = await opInvoke(projectId, "library.undo", {});
  return result.output as { applied: boolean; version: number };
}

export async function redoGlobalLibrary(
  projectId: string,
): Promise<{ applied: boolean; version: number }> {
  const result = await opInvoke(projectId, "library.redo", {});
  return result.output as { applied: boolean; version: number };
}

export async function collectToGlobalLibrary(
  projectId: string,
  items: Array<{ asset: string; name?: string }>,
): Promise<{ items: Array<{ id: string; name: string }>; version: number }> {
  const result = await opInvoke(projectId, "library.collect", { items });
  return result.output as { items: Array<{ id: string; name: string }>; version: number };
}

/**
 * `asset.import` succeeded but `collection.create` did not: the files are
 * already project assets, only the grouping and the board card are missing.
 */
export class PartialLibraryImportError extends Error {
  readonly importedCount: number;

  constructor(importedCount: number, cause: unknown) {
    super(cause instanceof Error ? cause.message : String(cause));
    this.name = "PartialLibraryImportError";
    this.importedCount = importedCount;
  }
}

export async function placeLibraryItems(
  projectId: string,
  items: LibraryItem[],
  position: { x: number; y: number },
  name?: string,
): Promise<{
  assets: string[];
  collectionId: string | null;
  boardItemId: string;
  historyEntryId: string | null;
}> {
  const resolvedName = name ?? (items.length > 1 ? formatLibraryImportName(items) : undefined);
  const result = await opInvoke(projectId, "library.place", {
    library_items: items.map((i) => i.id),
    position,
    ...(resolvedName ? { name: resolvedName } : {}),
  });
  const output = result.output as {
    assets: string[];
    collection_id?: string | null;
    board_item_id: string;
    history_entry_id?: string | null;
  };
  return {
    assets: output.assets ?? [],
    collectionId: output.collection_id ?? null,
    boardItemId: output.board_item_id,
    historyEntryId: output.history_entry_id ?? null,
  };
}

export async function importLibraryItemsToProject(
  projectId: string,
  items: LibraryItem[],
): Promise<{ collectionId: string; boardItemId?: string }> {
  const importResult = await opInvoke(projectId, "asset.import", {
    library_items: items.map((i) => i.id),
  });
  const assets = (importResult.output as { assets: string[] }).assets;
  const name = formatLibraryImportName(items);
  let collectionResult;
  try {
    collectionResult = await opInvoke(projectId, "collection.create", { name, assets });
  } catch (error) {
    throw new PartialLibraryImportError(assets.length, error);
  }
  const output = collectionResult.output as {
    collection_id: string;
    board_item_id?: string;
  };
  return {
    collectionId: output.collection_id,
    boardItemId: output.board_item_id,
  };
}

/* ───── Workspace Folders ───── */

export async function listWorkspaceFolders(projectId: string): Promise<WorkspaceFolder[]> {
  const result = await opInvoke(projectId, "project.list_folders", {});
  return WorkspaceFolderListSchema.parse(result.output).folders;
}

export async function addWorkspaceFolder(
  projectId: string,
  path: string,
): Promise<WorkspaceFolder> {
  const result = await opInvoke(projectId, "project.add_folder", { path });
  const errorDiag = (
    result.step as { diagnostics?: Array<{ severity: string; message: string }> }
  )?.diagnostics?.find((d) => d.severity === "error");
  if (errorDiag) {
    throw new Error(errorDiag.message);
  }
  return (result.output as { folder: WorkspaceFolder }).folder;
}

export async function removeWorkspaceFolder(projectId: string, id: string): Promise<void> {
  await opInvoke(projectId, "project.remove_folder", { id });
}
