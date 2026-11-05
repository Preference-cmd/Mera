import { useCallback, useEffect } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import * as m from "$paraglide/messages";
import { subscribeEvents } from "@/ipc";
import {
  PartialLibraryImportError,
  addGlobalLibraryFiles,
  collectToGlobalLibrary,
  importLibraryItemsToProject,
  listGlobalLibrary,
  redoGlobalLibrary,
  removeGlobalLibraryItems,
  renameGlobalLibraryItem,
  tagGlobalLibraryItem,
  undoGlobalLibrary,
} from "@/lib/globalLibrary";
import { positionCollectionOnBoard } from "@/lib/boardPlacement";
import type { LibraryItem } from "@/ipc/schemas";

/** Toast text for a failed library import; a partial import says the files did land. */
export function libraryImportErrorMessage(error: unknown): string {
  if (error instanceof PartialLibraryImportError) {
    return m["assets.importPartial"]({ count: error.importedCount });
  }
  return error instanceof Error ? error.message : m["assets.importFailed"]();
}

export const GLOBAL_LIBRARY_QUERY_KEY = ["global-library", "items"] as const;

export function useGlobalLibrary(projectId: string) {
  const queryClient = useQueryClient();

  const query = useQuery({
    queryKey: GLOBAL_LIBRARY_QUERY_KEY,
    queryFn: () => listGlobalLibrary(projectId, { include_removed: false }),
    staleTime: 0,
    refetchOnMount: "always",
  });

  useEffect(() => {
    const unsubscribe = subscribeEvents((event) => {
      if (event.type === "document" && event.kind === "library.changed") {
        void queryClient.invalidateQueries({ queryKey: GLOBAL_LIBRARY_QUERY_KEY });
      }
    });
    return () => unsubscribe();
  }, [queryClient]);

  const addFiles = useCallback(
    async (paths: string[]) => {
      const res = await addGlobalLibraryFiles(projectId, paths);
      await queryClient.invalidateQueries({ queryKey: GLOBAL_LIBRARY_QUERY_KEY });
      return res;
    },
    [projectId, queryClient],
  );

  const rename = useCallback(
    async (id: string, name: string) => {
      const res = await renameGlobalLibraryItem(projectId, id, name);
      await queryClient.invalidateQueries({ queryKey: GLOBAL_LIBRARY_QUERY_KEY });
      return res;
    },
    [projectId, queryClient],
  );

  const setTags = useCallback(
    async (id: string, tags: string[]) => {
      const res = await tagGlobalLibraryItem(projectId, id, tags);
      await queryClient.invalidateQueries({ queryKey: GLOBAL_LIBRARY_QUERY_KEY });
      return res;
    },
    [projectId, queryClient],
  );

  const remove = useCallback(
    async (ids: string[]) => {
      const res = await removeGlobalLibraryItems(projectId, ids);
      await queryClient.invalidateQueries({ queryKey: GLOBAL_LIBRARY_QUERY_KEY });
      return res;
    },
    [projectId, queryClient],
  );

  const undo = useCallback(async () => {
    const res = await undoGlobalLibrary(projectId);
    await queryClient.invalidateQueries({ queryKey: GLOBAL_LIBRARY_QUERY_KEY });
    return res;
  }, [projectId, queryClient]);

  const redo = useCallback(async () => {
    const res = await redoGlobalLibrary(projectId);
    await queryClient.invalidateQueries({ queryKey: GLOBAL_LIBRARY_QUERY_KEY });
    return res;
  }, [projectId, queryClient]);

  const collect = useCallback(
    async (items: Array<{ asset: string; name?: string }>) => {
      const res = await collectToGlobalLibrary(projectId, items);
      await queryClient.invalidateQueries({ queryKey: GLOBAL_LIBRARY_QUERY_KEY });
      return res;
    },
    [projectId, queryClient],
  );

  const importToProject = useCallback(
    async (items: LibraryItem[], dropPoint?: { x: number; y: number }) => {
      const { collectionId, boardItemId } = await importLibraryItemsToProject(projectId, items);
      await positionCollectionOnBoard(projectId, collectionId, boardItemId, dropPoint);
      return { collectionId, boardItemId };
    },
    [projectId],
  );

  return {
    items: query.data ?? [],
    isLoading: query.isLoading,
    error: query.error as Error | null,
    refetch: () => query.refetch(),
    addFiles,
    rename,
    setTags,
    remove,
    undo,
    redo,
    collect,
    importToProject,
  };
}
