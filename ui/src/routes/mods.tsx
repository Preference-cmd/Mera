import { createFileRoute } from "@tanstack/react-router";
import { ModsView } from "@/components/layout/ModsView";

export const Route = createFileRoute("/mods")({
  component: ModsView,
});
