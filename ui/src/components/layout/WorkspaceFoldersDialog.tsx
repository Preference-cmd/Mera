import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Folder, Plus, Trash2 } from "lucide-react";
import { toast } from "sonner";
import * as m from "$paraglide/messages";
import { ICON_STROKE } from "@/design/tokens";
import { isTauri } from "@/ipc";
import {
  addWorkspaceFolder,
  listWorkspaceFolders,
  removeWorkspaceFolder,
} from "@/lib/globalLibrary";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";

export function WorkspaceFoldersDialog({
  open,
  onOpenChange,
  projectId,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  projectId: string;
}) {
  const queryClient = useQueryClient();
  const [busy, setBusy] = useState(false);
  const tauri = isTauri();

  const { data: folders = [], isLoading } = useQuery({
    queryKey: ["workspace-folders", projectId],
    queryFn: () => listWorkspaceFolders(projectId),
    enabled: open && Boolean(projectId),
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

  const handleRemove = async (folderId: string) => {
    if (!window.confirm(m["folders.removeConfirm"]())) return;
    try {
      setBusy(true);
      await removeWorkspaceFolder(projectId, folderId);
      await queryClient.invalidateQueries({ queryKey: ["workspace-folders", projectId] });
      toast.success(m["folders.removed"]());
    } catch (err) {
      toast.error(err instanceof Error ? err.message : m["folders.removeFailed"]());
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-md" data-testid="workspace-folders-dialog">
        <DialogHeader>
          <DialogTitle>{m["folders.title"]()}</DialogTitle>
          <DialogDescription>{m["folders.description"]()}</DialogDescription>
        </DialogHeader>

        <div className="my-2 max-h-60 overflow-y-auto space-y-2">
          {isLoading ? (
            <div className="py-6 text-center text-xs text-fg3">Loading…</div>
          ) : folders.length === 0 ? (
            <div className="py-6 text-center text-xs text-fg3">{m["folders.empty"]()}</div>
          ) : (
            folders.map((folder) => (
              <div
                key={folder.id}
                className="flex items-center justify-between gap-2 rounded-md border border-hair bg-tile px-3 py-2 text-ui"
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
                  onClick={() => void handleRemove(folder.id)}
                  disabled={busy}
                  aria-label={m["folders.remove"]()}
                  className="rounded p-1 text-fg3 hover:bg-hover hover:text-danger focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
                >
                  <Trash2 className="size-3.5" strokeWidth={ICON_STROKE} />
                </button>
              </div>
            ))
          )}
        </div>

        <DialogFooter className="flex items-center justify-between sm:justify-between">
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => void handleAddFolder()}
            disabled={busy}
            className="gap-1.5"
            data-testid="add-workspace-folder-button"
          >
            <Plus className="size-3.5" strokeWidth={ICON_STROKE} />
            {m["folders.add"]()}
          </Button>
          <Button type="button" size="sm" onClick={() => onOpenChange(false)}>
            Close
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
