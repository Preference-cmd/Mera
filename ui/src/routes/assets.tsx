import { createFileRoute } from "@tanstack/react-router";
import { AssetLibraryView } from "@/components/layout/AssetLibraryView";
import { isAssetLibrarySort, type AssetLibrarySort } from "@/lib/globalLibrary";

export const Route = createFileRoute("/assets")({
  validateSearch: (search: Record<string, unknown>): { sort?: AssetLibrarySort } => ({
    sort: isAssetLibrarySort(search.sort) ? search.sort : undefined,
  }),
  component: AssetLibraryView,
});
