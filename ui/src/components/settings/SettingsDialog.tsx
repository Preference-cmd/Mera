import { useLayoutEffect, useRef, type ReactNode } from "react";
import { X } from "lucide-react";
import * as m from "$paraglide/messages";
import { cn } from "@/lib/utils";
import { ICON_STROKE } from "@/design/tokens";
import { useProjectStore } from "@/store/project";
import { useUIStore, type SettingsSection } from "@/store/uiStore";
import { Dialog, DialogClose, DialogContent, DialogTitle } from "@/components/ui/dialog";
import { ShellIconButton } from "@/components/layout/shellChrome";
import {
  AboutSection,
  AppearanceSection,
  GeneralSection,
  ShortcutsSection,
} from "./AppSettingsSections";
import { ModsSettingsSection } from "./ModsSettingsSection";
import { WorkspaceFolders } from "./WorkspaceFolders";
import { WorkspaceGeneralSection } from "./WorkspaceGeneralSection";

type NavItem = { id: SettingsSection; label: () => string };

const APP_ITEMS: NavItem[] = [
  { id: "general", label: () => m["settings.general"]() },
  { id: "appearance", label: () => m["settings.appearance"]() },
  { id: "shortcuts", label: () => m["settings.shortcuts"]() },
  { id: "mods", label: () => m["settings.mods"]() },
  { id: "about", label: () => m["settings.about"]() },
];

const WORKSPACE_ITEMS: NavItem[] = [
  { id: "workspace-general", label: () => m["settings.general"]() },
  { id: "workspace-folders", label: () => m["settings.folders"]() },
];

function isWorkspaceSection(section: SettingsSection): boolean {
  return section === "workspace-general" || section === "workspace-folders";
}

/**
 * The settings dialog, mounted once by the shell and driven by the UI store.
 * It floats over the current route behind a plain dim scrim, so theme changes
 * show through and the canvas underneath keeps its viewport and selection.
 * Every control applies immediately; there is nothing to save.
 */
export function SettingsDialog() {
  const open = useUIStore((s) => s.settingsOpen);
  const storedSection = useUIStore((s) => s.settingsSection);
  const targetWorkspaceId = useUIStore((s) => s.settingsWorkspaceId);
  const setSection = useUIStore((s) => s.setSettingsSection);
  const closeSettings = useUIStore((s) => s.closeSettings);
  const projects = useProjectStore((s) => s.projects);
  const activeProjectId = useProjectStore((s) => s.activeProjectId);

  // Radix returns focus to whatever was focused at open. A menu item is gone
  // by close time, so remember the menu's trigger instead. Layout effects run
  // before the dialog moves focus inside itself.
  const openerRef = useRef<HTMLElement | null>(null);
  useLayoutEffect(() => {
    if (!open) return;
    const active = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const triggerId = active?.closest('[role="menu"]')?.getAttribute("aria-labelledby");
    openerRef.current = (triggerId && document.getElementById(triggerId)) || active;
  }, [open]);

  const workspaceId = targetWorkspaceId ?? activeProjectId;
  const workspace = projects.find((p) => p.id === workspaceId) ?? null;
  const workspaceGroupLabel = workspace
    ? m["settings.workspaceGroup"]({ name: workspace.name })
    : m["settings.workspace"]();

  // With no workspace to show, a remembered workspace section falls back to
  // General instead of opening on a disabled page.
  const section: SettingsSection =
    isWorkspaceSection(storedSection) && !workspace ? "general" : storedSection;

  const allItems = [...APP_ITEMS, ...WORKSPACE_ITEMS];
  const sectionLabel = allItems.find((item) => item.id === section)?.label() ?? "";

  let content: ReactNode;
  if (section === "workspace-general" && workspace) {
    content = <WorkspaceGeneralSection projectId={workspace.id} name={workspace.name} />;
  } else if (section === "workspace-folders" && workspace) {
    content = <WorkspaceFolders projectId={workspace.id} />;
  } else if (section === "appearance") {
    content = <AppearanceSection />;
  } else if (section === "shortcuts") {
    content = <ShortcutsSection />;
  } else if (section === "mods") {
    content = <ModsSettingsSection />;
  } else if (section === "about") {
    content = <AboutSection />;
  } else {
    content = <GeneralSection />;
  }

  return (
    <Dialog open={open} onOpenChange={(next) => !next && closeSettings()}>
      <DialogContent
        showCloseButton={false}
        aria-describedby={undefined}
        data-testid="settings-dialog"
        onCloseAutoFocus={(event) => {
          const opener = openerRef.current;
          openerRef.current = null;
          if (opener?.isConnected) {
            event.preventDefault();
            opener.focus();
          }
        }}
        onEscapeKeyDown={(event) => {
          // Esc inside an edited name field reverts the edit; the next Esc closes.
          const active = document.activeElement as HTMLElement | null;
          if (active?.dataset.settingsEscapeReverts === "true") event.preventDefault();
        }}
        className="flex h-[640px] max-h-[85vh] w-[900px] max-w-[90vw] gap-0 overflow-hidden rounded-xl bg-ground p-0 sm:max-w-[90vw]"
      >
        <DialogTitle className="sr-only">{m["shell.settings"]()}</DialogTitle>
        <nav
          aria-label={m["shell.settings"]()}
          className="w-[200px] shrink-0 overflow-y-auto border-r border-hair-soft px-3 py-6"
        >
          <NavGroup
            label={m["settings.groupApp"]()}
            items={APP_ITEMS}
            active={section}
            onSelect={setSection}
          />
          <NavGroup
            label={workspaceGroupLabel}
            items={WORKSPACE_ITEMS}
            active={section}
            onSelect={setSection}
            disabled={!workspace}
            hint={workspace ? undefined : m["settings.noWorkspace"]()}
          />
        </nav>

        <div className="min-h-0 min-w-0 flex-1 overflow-y-auto">
          <div className="w-full px-7 py-8">
            {isWorkspaceSection(section) && workspace && (
              <div className="mb-1 truncate text-caption text-fg3">{workspaceGroupLabel}</div>
            )}
            <h2 className="mb-6 text-headline-lg text-fg">{sectionLabel}</h2>
            {content}
          </div>
        </div>

        <DialogClose asChild>
          <ShellIconButton label={m["common.close"]()} className="absolute top-3 right-3">
            <X className="size-4" strokeWidth={ICON_STROKE} />
          </ShellIconButton>
        </DialogClose>
      </DialogContent>
    </Dialog>
  );
}

function NavGroup({
  label,
  items,
  active,
  onSelect,
  disabled = false,
  hint,
}: {
  label: string;
  items: NavItem[];
  active: SettingsSection;
  onSelect: (id: SettingsSection) => void;
  disabled?: boolean;
  hint?: string;
}) {
  return (
    <div
      role="group"
      aria-label={label}
      aria-disabled={disabled || undefined}
      className="mb-5 last:mb-0"
    >
      <div className="truncate px-2.5 pb-1.5 text-xs font-medium tracking-[0.02em] text-fg4">
        {label}
      </div>
      <div className="space-y-0.5">
        {items.map((item) => {
          const current = item.id === active;
          return (
            <button
              key={item.id}
              type="button"
              disabled={disabled}
              aria-current={current && !disabled ? "page" : undefined}
              onClick={() => onSelect(item.id)}
              className={cn(
                "flex h-8 w-full cursor-pointer items-center rounded-sm px-2.5 text-left text-ui transition-colors duration-150 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent disabled:cursor-default disabled:opacity-45 disabled:hover:bg-transparent",
                current && !disabled
                  ? "bg-active font-medium text-fg"
                  : "text-fg2 hover:bg-hover hover:text-fg",
              )}
            >
              <span className="min-w-0 flex-1 truncate">{item.label()}</span>
            </button>
          );
        })}
      </div>
      {hint && <p className="px-2.5 pt-1.5 text-caption text-fg3">{hint}</p>}
    </div>
  );
}
