import { createFileRoute } from "@tanstack/react-router";
import { AssetLibraryView } from "@/components/layout/AssetLibraryView";

export const Route = createFileRoute("/assets")({
  component: AssetLibraryView,
});
