import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Folder, Plus, Trash2 } from "lucide-react";
import { toast } from "sonner";
import * as m from "$paraglide/messages";
import { ICON_STROKE } from "@/design/tokens";
import { cn } from "@/lib/utils";
import { isTauri } from "@/ipc";
import {
  addWorkspaceFolder,
  listWorkspaceFolders,
  removeWorkspaceFolder,
} from "@/lib/globalLibrary";
import { Button } from "@/components/ui/button";

/**
 * The workspace folders of one project: the only places the agent may read or
 * import files from. Adding uses the desktop directory picker; backend
 * diagnostics surface as error toasts.
 */
export function WorkspaceFolders({ projectId }: { projectId: string }) {
  const queryClient = useQueryClient();
  const [busy, setBusy] = useState(false);
  const tauri = isTauri();

  const { data: folders = [], isLoading } = useQuery({
    queryKey: ["workspace-folders", projectId],
    queryFn: () => listWorkspaceFolders(projectId),
    enabled: Boolean(projectId),
    staleTime: 0,
  });

  const handleAddFolder = async () => {
    if (!tauri) {
      toast.info(m["folders.desktopOnly"]());
      return;
    }
    try {
      const { open: openDialog } = await import("@tauri-apps/plugin-dialog");
      const selected = await openDialog({ directory: true, multiple: false });
      if (!selected || typeof selected !== "string") return;

      setBusy(true);
      await addWorkspaceFolder(projectId, selected);
      await queryClient.invalidateQueries({ queryKey: ["workspace-folders", projectId] });
      toast.success(m["folders.added"]());
    } catch (err) {
      toast.error(err instanceof Error ? err.message : m["folders.addFailed"]());
    } finally {
      setBusy(false);
    }
  };

  const handleReAdd = async (path: string) => {
    try {
      setBusy(true);
      await addWorkspaceFolder(projectId, path);
      await queryClient.invalidateQueries({ queryKey: ["workspace-folders", projectId] });
      toast.success(m["folders.added"]());
    } catch (err) {
      toast.error(err instanceof Error ? err.message : m["folders.addFailed"]());
    } finally {
      setBusy(false);
    }
  };

  const handleRemove = async (folder: { id: string; path: string }) => {
    try {
      setBusy(true);
      await removeWorkspaceFolder(projectId, folder.id);
      await queryClient.invalidateQueries({ queryKey: ["workspace-folders", projectId] });
      toast.success(m["folders.removed"](), {
        duration: 8000,
        action: {
          label: m["assets.undo"](),
          onClick: () => void handleReAdd(folder.path),
        },
      });
    } catch (err) {
      toast.error(err instanceof Error ? err.message : m["folders.removeFailed"]());
    } finally {
      setBusy(false);
    }
  };

  return (
    <div data-testid="workspace-folders">
      <p className="mb-3 text-caption leading-relaxed text-fg2">{m["folders.description"]()}</p>
      <div className="overflow-hidden rounded-xl border border-hair bg-surface">
        {isLoading ? (
          <div className="px-5 py-6 text-center text-caption text-fg3">{m["common.loading"]()}</div>
        ) : folders.length === 0 ? (
          <div className="px-5 py-6 text-center text-caption text-fg3">{m["folders.empty"]()}</div>
        ) : (
          <ul aria-label={m["folders.title"]()}>
            {folders.map((folder, idx) => (
              <li
                key={folder.id}
                className={cn(
                  "flex items-center justify-between gap-2 px-4 py-2.5",
                  idx > 0 && "border-t border-hair-soft",
                )}
                data-testid="workspace-folder-row"
              >
                <div className="flex min-w-0 items-center gap-2">
                  <Folder className="size-4 shrink-0 text-fg3" strokeWidth={ICON_STROKE} />
                  <span className="truncate font-mono text-xs text-fg" title={folder.path}>
                    {folder.path}
                  </span>
                </div>
                <button
                  type="button"
                  onClick={() => void handleRemove(folder)}
                  disabled={busy}
                  aria-label={m["folders.remove"]()}
                  className="rounded p-1 text-fg3 hover:bg-hover hover:text-err focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
                >
                  <Trash2 className="size-3.5" strokeWidth={ICON_STROKE} />
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>
      <Button
        type="button"
        variant="outline"
        size="sm"
        onClick={() => void handleAddFolder()}
        disabled={busy}
        className="mt-3 gap-1.5"
        data-testid="add-workspace-folder-button"
      >
        <Plus className="size-3.5" strokeWidth={ICON_STROKE} />
        {m["folders.add"]()}
      </Button>
    </div>
  );
}
