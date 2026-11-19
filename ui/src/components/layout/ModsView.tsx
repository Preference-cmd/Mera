import * as m from "$paraglide/messages";
import { ModsSettingsSection } from "@/components/settings/ModsSettingsSection";

/**
 * ModsView — full-page Mods manager reached from the sidebar rail.
 * Manages installed mods, remote registry browsing, and trusted sources.
 */
export function ModsView() {
  return (
    <div className="h-full min-h-0 overflow-y-auto bg-ground" data-testid="mods-view">
      <div className="mx-auto w-full max-w-4xl px-8 py-7">
        <h1 className="mb-1 text-display-sm font-semibold text-fg">{m["settings.mods"]()}</h1>
        <ModsSettingsSection />
      </div>
    </div>
  );
}
