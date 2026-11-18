import { useCallback, useEffect, useMemo, useState, type ComponentType } from "react";
import {
  Check,
  FileBox,
  FileText,
  Film,
  FolderPlus,
  Grid,
  Image as ImageIcon,
  List,
  MoreHorizontal,
  Music,
  Plus,
  Search,
  Tag,
  Trash2,
  Upload,
} from "lucide-react";
import { useNavigate, useSearch } from "@tanstack/react-router";
import { toast } from "sonner";
import * as m from "$paraglide/messages";
import { getLocale } from "$paraglide/runtime";
import { ICON_STROKE } from "@/design/tokens";
import { cn } from "@/lib/utils";
import { formatBytes } from "@/lib/library";
import { formatRelativeTime } from "@/components/layout/shellChrome";
import {
  extractUniqueTags,
  filterLibraryItems,
  friendlyMediaTypeLabel,
  getMediaTypeGroup,
  isAssetLibrarySort,
  sortLibraryItems,
  type AssetLibrarySort,
  type MediaTypeFilter,
} from "@/lib/globalLibrary";
import { ImageThumbnail } from "@/components/ui/ImageThumbnail";
import { libraryImportErrorMessage, useGlobalLibrary } from "@/hooks/useGlobalLibrary";
import { useProjectStore } from "@/store/project";
import { isShellModalOpen, useUIStore } from "@/store/uiStore";
import { getLibraryAssetUrl, isTauri, readLibraryAssetPreview, storagePreflightWrite } from "@/ipc";
import type { LibraryItem } from "@/ipc/schemas";
import {
  StoragePreflightDialog,
  type PendingStoragePreflight,
} from "@/components/storage/StoragePreflightDialog";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";

const MEDIA_ICONS: Record<string, ComponentType<{ className?: string; strokeWidth?: number }>> = {
  image: ImageIcon,
  text: FileText,
  video: Film,
  audio: Music,
  other: FileBox,
};

function TextThumbnail({ assetId }: { assetId: string }) {
  const [text, setText] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    void readLibraryAssetPreview(assetId, 8)
      .then((prev) => {
        if (active) setText(prev.text);
      })
      .catch(() => {
        if (active) setText(null);
      });
    return () => {
      active = false;
    };
  }, [assetId]);

  return (
    <div className="h-full w-full overflow-hidden bg-tile p-2 font-mono text-[9px] leading-tight text-fg3 select-none">
      {text ? (
        <pre className="whitespace-pre-wrap font-inherit">{text}</pre>
      ) : (
        <span className="italic">{m["assets.loadingPreview"]()}</span>
      )}
    </div>
  );
}

export function AssetLibraryView() {
  const projectId = useProjectStore((state) => state.activeProjectId);
  const navigate = useNavigate();
  const search = useSearch({ from: "/assets" });
  const tauri = isTauri();

  const viewMode = useUIStore((state) => state.assetLibraryViewMode);
  const setViewMode = useUIStore((state) => state.setAssetLibraryViewMode);
  const requestCanvasFocus = useUIStore((state) => state.requestCanvasFocus);

  const {
    items,
    isLoading,
    addFiles,
    rename,
    setTags,
    remove,
    purge,
    undo,
    redo,
    importToProject,
  } = useGlobalLibrary(projectId);

  const urlSort: AssetLibrarySort = isAssetLibrarySort(search.sort) ? search.sort : "recent";
  const [sortBy, setSortBy] = useState<AssetLibrarySort>(urlSort);
  const [query, setQuery] = useState("");
  const [selectedTag, setSelectedTag] = useState<string | null>(null);
  const [mediaTypeFilter, setMediaTypeFilter] = useState<MediaTypeFilter>("all");
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const [isDraggingOver, setIsDraggingOver] = useState(false);
  const [importing, setImporting] = useState(false);
  const [purgeDialogOpen, setPurgeDialogOpen] = useState(false);
  const [purging, setPurging] = useState(false);
  const [pendingAddPaths, setPendingAddPaths] = useState<string[] | null>(null);
  const [pendingPreflight, setPendingPreflight] = useState<PendingStoragePreflight | null>(null);
  const [addingBusy, setAddingBusy] = useState(false);

  // Dialogs for rename and edit tags
  const [editingItem, setEditingItem] = useState<{ id: string; name: string } | null>(null);
  const [taggingItem, setTaggingItem] = useState<{ id: string; tags: string } | null>(null);

  useEffect(() => {
    setSortBy(urlSort);
  }, [urlSort]);

  const uniqueTags = useMemo(() => extractUniqueTags(items), [items]);
  const visibleItems = useMemo(
    () => sortLibraryItems(filterLibraryItems(items, query, selectedTag, mediaTypeFilter), sortBy),
    [items, query, selectedTag, mediaTypeFilter, sortBy],
  );

  const handleUndo = useCallback(async () => {
    try {
      const res = await undo();
      if (res.applied) {
        toast.success(m["assets.undone"]());
      }
    } catch {
      toast.error(m["assets.undoFailed"]());
    }
  }, [undo]);

  const handleRedo = useCallback(async () => {
    try {
      const res = await redo();
      if (res.applied) {
        toast.success(m["assets.redone"]());
      }
    } catch {
      toast.error(m["assets.redoFailed"]());
    }
  }, [redo]);

  // Wire Cmd/Ctrl+Z and Shift+Cmd/Ctrl+Z shortcuts
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (isShellModalOpen()) return;
      const target = e.target as HTMLElement | null;
      if (
        target &&
        (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable)
      ) {
        return;
      }
      const mod = e.metaKey || e.ctrlKey;
      if (!mod) return;

      if (e.key === "z" || e.key === "Z") {
        e.preventDefault();
        e.stopPropagation();
        if (e.shiftKey) {
          void handleRedo();
        } else {
          void handleUndo();
        }
      } else if (e.key === "y" || e.key === "Y") {
        e.preventDefault();
        e.stopPropagation();
        void handleRedo();
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [handleUndo, handleRedo]);

  const executeAddPaths = useCallback(
    async (paths: string[]) => {
      const res = await addFiles(paths);
      const deduplicated = res.items.filter((i) => i.deduplicated).length;
      toast.success(
        m["assets.added"]({
          count: res.items.length,
          deduplicated,
        }),
      );
    },
    [addFiles],
  );

  const runAddWithPreflight = useCallback(
    async (paths: string[], estimatedBytes = 0) => {
      try {
        const preflight = await storagePreflightWrite({
          kind: "library",
          estimatedBytes,
          paths,
        });
        if (preflight.requiresConfirm) {
          const itemLabel =
            paths.length === 1
              ? (paths[0].split(/[/\\]/).pop() ?? paths[0])
              : `${paths.length} files`;
          setPendingAddPaths(paths);
          setPendingPreflight({
            itemLabel,
            estimatedBytes,
            result: preflight,
          });
          return;
        }
        await executeAddPaths(paths);
      } catch (err) {
        toast.error(err instanceof Error ? err.message : m["assets.addFailed"]());
      }
    },
    [executeAddPaths],
  );

  const handleConfirmPreflightAdd = async () => {
    if (!pendingAddPaths || addingBusy) return;
    setAddingBusy(true);
    try {
      await executeAddPaths(pendingAddPaths);
      setPendingAddPaths(null);
      setPendingPreflight(null);
    } catch (err) {
      toast.error(err instanceof Error ? err.message : m["assets.addFailed"]());
    } finally {
      setAddingBusy(false);
    }
  };

  // Tauri OS file drag-drop event
  useEffect(() => {
    if (!tauri) return;
    // Registration is async: if the view unmounts first, drop the listener as
    // soon as it arrives, or a later file drop on another route would still
    // add to the library.
    let disposed = false;
    let unlisten: (() => void) | undefined;
    import("@tauri-apps/api/webview")
      .then(({ getCurrentWebview }) => {
        return getCurrentWebview().onDragDropEvent((event) => {
          const payload = event.payload;
          if (payload.type === "enter" || payload.type === "over") {
            setIsDraggingOver(true);
          } else if (payload.type === "leave") {
            setIsDraggingOver(false);
          } else if (payload.type === "drop") {
            setIsDraggingOver(false);
            const paths = payload.paths;
            if (Array.isArray(paths) && paths.length > 0) {
              void runAddWithPreflight(paths, 0);
            }
          }
        });
      })
      .then((fn) => {
        if (disposed) fn();
        else unlisten = fn;
      })
      .catch(() => {});

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [tauri, runAddWithPreflight]);

  const handleAddFiles = async () => {
    if (!tauri) {
      toast.info(m["assets.addFilesUnavailable"]());
      return;
    }
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const selection = await open({ multiple: true, directory: false });
      if (!selection) return;
      const paths = Array.isArray(selection) ? selection : [selection];
      if (paths.length === 0) return;

      await runAddWithPreflight(paths, 0);
    } catch (err) {
      toast.error(err instanceof Error ? err.message : m["assets.addFailed"]());
    }
  };

  const handlePurgeRemoved = async () => {
    if (purging) return;
    setPurging(true);
    try {
      const res = await purge();
      setPurgeDialogOpen(false);
      if (res.purged_entries > 0) {
        toast.success(
          m["storage.purgedToast"]({
            count: res.purged_entries,
            size: formatBytes(res.freed_bytes),
          }),
        );
      } else {
        toast.info(m["storage.purgeNothingToast"]());
      }
    } catch (err) {
      toast.error(err instanceof Error ? err.message : m["storage.purgeFailed"]());
    } finally {
      setPurging(false);
    }
  };

  const handleRenameSave = async () => {
    if (!editingItem) return;
    const trimmed = editingItem.name.trim();
    if (!trimmed) return;
    try {
      await rename(editingItem.id, trimmed);
      setEditingItem(null);
      toast.success(m["assets.renamed"](), {
        action: {
          label: m["assets.undo"](),
          onClick: () => void handleUndo(),
        },
      });
    } catch (err) {
      toast.error(err instanceof Error ? err.message : m["assets.renameFailed"]());
    }
  };

  const handleTagsSave = async () => {
    if (!taggingItem) return;
    const tags = taggingItem.tags
      .split(",")
      .map((t) => t.trim())
      .filter(Boolean);
    try {
      await setTags(taggingItem.id, tags);
      setTaggingItem(null);
      toast.success(m["assets.tagsUpdated"](), {
        action: {
          label: m["assets.undo"](),
          onClick: () => void handleUndo(),
        },
      });
    } catch (err) {
      toast.error(err instanceof Error ? err.message : m["assets.tagsFailed"]());
    }
  };

  const handleRemoveItems = async (ids: string[]) => {
    if (ids.length === 0) return;
    try {
      await remove(ids);
      setSelectedIds((prev) => {
        const next = new Set(prev);
        for (const id of ids) next.delete(id);
        return next;
      });
      toast.success(
        ids.length === 1
          ? m["assets.removedOne"]()
          : m["assets.removedMany"]({ count: ids.length }),
        {
          duration: 8000,
          action: {
            label: m["assets.undo"](),
            onClick: () => void handleUndo(),
          },
        },
      );
    } catch (err) {
      toast.error(err instanceof Error ? err.message : m["assets.removeFailed"]());
    }
  };

  const handleImportSelected = async (targetItems: LibraryItem[]) => {
    if (targetItems.length === 0 || importing) return;
    setImporting(true);
    try {
      const { boardItemId } = await importToProject(targetItems);
      toast.success(m["assets.importSuccess"](), {
        action: {
          label: m["assets.showOnCanvas"](),
          onClick: () => {
            if (boardItemId) requestCanvasFocus(boardItemId);
            void navigate({ to: "/" });
          },
        },
      });
    } catch (err) {
      toast.error(libraryImportErrorMessage(err));
    } finally {
      setImporting(false);
    }
  };

  const toggleSelect = (id: string) => {
    setSelectedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const selectAll = () => {
    setSelectedIds(new Set(visibleItems.map((i) => i.id)));
  };

  const deselectAll = () => {
    setSelectedIds(new Set());
  };

  const selectedItems = useMemo(
    () => items.filter((i) => selectedIds.has(i.id)),
    [items, selectedIds],
  );

  return (
    <div
      className="relative flex h-full min-h-0 flex-col overflow-y-auto bg-surface px-6 pt-5 pb-12"
      data-testid="asset-library-view"
    >
      <h1 className="sr-only">{m["assets.title"]()}</h1>

      {/* Drag & drop overlay */}
      {isDraggingOver && (
        <div
          className="absolute inset-0 z-50 flex items-center justify-center bg-surface/90 backdrop-blur-sm border-2 border-dashed border-accent m-4 rounded-xl pointer-events-none"
          data-testid="drop-overlay"
        >
          <div className="flex flex-col items-center gap-2 text-accent">
            <Upload className="size-10" strokeWidth={ICON_STROKE} />
            <p className="text-body font-medium">{m["assets.dropOverlay"]()}</p>
          </div>
        </div>
      )}

      {/* Top Filter and Actions Row */}
      <div className="flex shrink-0 flex-wrap items-center justify-between gap-3">
        <div className="flex flex-wrap items-center gap-2">
          {/* Search box */}
          <label className="flex h-8 w-60 items-center gap-2 rounded-full px-3 text-fg3 shadow-[inset_0_0_0_1px_var(--color-hair-strong)] transition-shadow focus-within:text-fg2 focus-within:shadow-[inset_0_0_0_1px_var(--color-accent-line),0_0_0_4px_var(--color-accent-tint)]">
            <Search className="size-3.5 shrink-0" strokeWidth={ICON_STROKE} aria-hidden="true" />
            <input
              type="text"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              aria-label={m["assets.searchLabel"]()}
              placeholder={m["assets.searchPlaceholder"]()}
              spellCheck={false}
              data-testid="assets-search"
              className="min-w-0 flex-1 border-0 bg-transparent text-ui text-fg outline-none placeholder:text-fg2"
            />
          </label>

          {/* Media type chips */}
          <div role="group" className="flex items-center gap-1">
            {(["all", "image", "text", "video", "audio", "other"] as const).map((type) => {
              const active = mediaTypeFilter === type;
              const labels: Record<MediaTypeFilter, () => string> = {
                all: m["assets.filterAll"],
                image: m["assets.filterImage"],
                text: m["assets.filterText"],
                video: m["assets.filterVideo"],
                audio: m["assets.filterAudio"],
                other: m["assets.filterOther"],
              };
              return (
                <button
                  key={type}
                  type="button"
                  onClick={() => setMediaTypeFilter(type)}
                  className={cn(
                    "h-7 rounded-full px-2.5 text-caption font-medium transition-colors cursor-pointer",
                    active
                      ? "bg-accent-tint text-accent shadow-[inset_0_0_0_1px_var(--color-accent-line)]"
                      : "text-fg3 hover:bg-tile hover:text-fg",
                  )}
                  data-testid={`filter-${type}`}
                >
                  {labels[type]()}
                </button>
              );
            })}
          </div>
        </div>

        {/* Sort, View Mode Toggle, Add Files Button, and More Menu */}
        <div className="flex items-center gap-2">
          <select
            aria-label={m["assets.sortLabel"]()}
            value={sortBy}
            onChange={(e) => {
              const next = e.target.value as AssetLibrarySort;
              setSortBy(next);
              void navigate({
                to: "/assets",
                search: next === "recent" ? {} : { sort: next },
                replace: true,
              });
            }}
            data-testid="assets-sort-select"
            className="h-8 rounded-md border border-hair bg-surface px-2.5 text-caption text-fg focus:border-accent focus:outline-none"
          >
            <option value="recent">{m["assets.sortRecent"]()}</option>
            <option value="name">{m["assets.sortName"]()}</option>
            <option value="size">{m["assets.sortSize"]()}</option>
          </select>

          <div className="flex items-center rounded-md border border-hair bg-tile p-0.5">
            <button
              type="button"
              onClick={() => setViewMode("grid")}
              aria-label={m["assets.viewGrid"]()}
              className={cn(
                "rounded p-1 text-fg3 transition-colors cursor-pointer",
                viewMode === "grid" ? "bg-surface text-fg shadow-1" : "hover:text-fg",
              )}
              data-testid="view-grid-button"
            >
              <Grid className="size-3.5" strokeWidth={ICON_STROKE} />
            </button>
            <button
              type="button"
              onClick={() => setViewMode("list")}
              aria-label={m["assets.viewList"]()}
              className={cn(
                "rounded p-1 text-fg3 transition-colors cursor-pointer",
                viewMode === "list" ? "bg-surface text-fg shadow-1" : "hover:text-fg",
              )}
              data-testid="view-list-button"
            >
              <List className="size-3.5" strokeWidth={ICON_STROKE} />
            </button>
          </div>

          <Button
            type="button"
            size="sm"
            onClick={() => void handleAddFiles()}
            disabled={!tauri}
            className="gap-1.5"
            data-testid="add-files-button"
          >
            <Plus className="size-3.5" strokeWidth={ICON_STROKE} />
            {m["assets.addFiles"]()}
          </Button>

          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button
                type="button"
                size="sm"
                variant="outline"
                aria-label={m["common.moreOptions"]()}
                className="px-2"
                data-testid="assets-more-button"
              >
                <MoreHorizontal className="size-4" strokeWidth={ICON_STROKE} />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end" className="w-48">
              <DropdownMenuItem
                className="text-danger"
                onClick={() => setPurgeDialogOpen(true)}
                data-testid="assets-purge-removed-item"
              >
                <Trash2 className="mr-2 size-3.5" strokeWidth={ICON_STROKE} />
                {m["assets.purgeRemoved"]()}
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </div>

      {/* Tags row */}
      {uniqueTags.length > 0 && (
        <div className="mt-3 flex flex-wrap items-center gap-1.5">
          <Tag className="size-3.5 text-fg4 mr-1" strokeWidth={ICON_STROKE} />
          {uniqueTags.map((tag) => {
            const active = selectedTag === tag;
            return (
              <button
                key={tag}
                type="button"
                onClick={() => setSelectedTag(active ? null : tag)}
                className={cn(
                  "h-6 rounded-full px-2 text-xs transition-colors cursor-pointer",
                  active
                    ? "bg-accent text-accent-contrast"
                    : "border border-hair bg-tile text-fg3 hover:bg-hover hover:text-fg",
                )}
                data-testid={`tag-chip-${tag}`}
              >
                #{tag}
              </button>
            );
          })}
          {selectedTag && (
            <button
              type="button"
              onClick={() => setSelectedTag(null)}
              className="text-xs text-fg3 underline hover:text-fg ml-1 cursor-pointer"
            >
              {m["common.clear"]()}
            </button>
          )}
        </div>
      )}

      {/* Batch Selection Action Bar */}
      {selectedIds.size > 0 && (
        <div
          className="mt-4 flex items-center justify-between rounded-lg border border-hair-strong bg-tile px-4 py-2 text-ui shadow-1"
          data-testid="selection-bar"
        >
          <div className="flex items-center gap-3">
            <span className="font-medium text-fg">
              {m["assets.selectedCount"]({ count: selectedIds.size })}
            </span>
            <button
              type="button"
              onClick={deselectAll}
              className="text-caption text-fg3 hover:text-fg cursor-pointer underline"
            >
              {m["assets.deselectAll"]()}
            </button>
          </div>
          <div className="flex items-center gap-2">
            <Button
              type="button"
              size="sm"
              variant="outline"
              disabled={importing}
              onClick={() => void handleImportSelected(selectedItems)}
              className="gap-1.5"
              data-testid="import-selected-button"
            >
              <FolderPlus className="size-3.5" strokeWidth={ICON_STROKE} />
              {m["assets.importToProject"]()}
            </Button>
            <Button
              type="button"
              size="sm"
              variant="destructive"
              onClick={() => void handleRemoveItems(Array.from(selectedIds))}
              className="gap-1.5"
              data-testid="remove-selected-button"
            >
              <Trash2 className="size-3.5" strokeWidth={ICON_STROKE} />
              {m["assets.remove"]()}
            </Button>
          </div>
        </div>
      )}

      {/* Main Content: Loading, Empty, Grid, or List */}
      <div className="mt-4 min-h-0 flex-1">
        {isLoading ? (
          <div className="py-20 text-center text-ui text-fg3">{m["assets.loading"]()}</div>
        ) : visibleItems.length === 0 ? (
          <div
            className="flex flex-col items-center justify-center py-20 text-center"
            data-testid="assets-empty-state"
          >
            <p className="text-body font-medium text-fg">{m["assets.empty"]()}</p>
            <p className="mt-1 text-caption text-fg3">{m["assets.emptyHint"]()}</p>
          </div>
        ) : viewMode === "grid" ? (
          /* Grid View */
          <div
            className="grid grid-cols-2 gap-4 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6"
            data-testid="assets-grid"
          >
            {visibleItems.map((item) => {
              const selected = selectedIds.has(item.id);
              const group = getMediaTypeGroup(item.media_type);
              const Icon = MEDIA_ICONS[group] ?? FileBox;

              return (
                <div
                  key={item.id}
                  onClick={() => toggleSelect(item.id)}
                  className={cn(
                    "group relative flex flex-col overflow-hidden rounded-lg border border-hair bg-tile transition-all duration-150 cursor-pointer select-none hover:border-hair-strong hover:shadow-2",
                    selected && "border-accent ring-2 ring-accent",
                  )}
                  data-testid="asset-card"
                  data-asset-id={item.id}
                >
                  {/* Thumbnail container */}
                  <div className="relative aspect-video w-full overflow-hidden bg-tile-subtle">
                    {group === "image" ? (
                      <ImageThumbnail
                        src={getLibraryAssetUrl(item.id)}
                        alt={item.name}
                        loading="lazy"
                        icon={Icon}
                        className="h-full w-full object-cover"
                      />
                    ) : group === "text" ? (
                      <TextThumbnail assetId={item.id} />
                    ) : (
                      <div className="flex h-full w-full items-center justify-center text-fg3">
                        <Icon className="size-8" strokeWidth={ICON_STROKE} />
                      </div>
                    )}

                    {/* Selection tick checkbox */}
                    <div
                      className={cn(
                        "absolute top-2 left-2 flex size-5 items-center justify-center rounded border border-hair bg-surface text-accent shadow-1 transition-opacity",
                        selected ? "opacity-100" : "opacity-0 group-hover:opacity-100",
                      )}
                    >
                      {selected && <Check className="size-3.5" strokeWidth={3} />}
                    </div>

                    {/* Card row menu */}
                    <div
                      className="absolute top-2 right-2 opacity-0 group-hover:opacity-100 transition-opacity"
                      onClick={(e) => e.stopPropagation()}
                    >
                      <DropdownMenu>
                        <DropdownMenuTrigger asChild>
                          <button
                            type="button"
                            aria-label={m["common.moreOptions"]()}
                            className="flex size-6 items-center justify-center rounded bg-surface/80 p-0.5 text-fg shadow-1 backdrop-blur hover:bg-surface focus-visible:opacity-100"
                          >
                            <MoreHorizontal className="size-3.5" strokeWidth={ICON_STROKE} />
                          </button>
                        </DropdownMenuTrigger>
                        <DropdownMenuContent align="end" className="w-44">
                          <DropdownMenuItem onClick={() => void handleImportSelected([item])}>
                            <FolderPlus className="mr-2 size-3.5" strokeWidth={ICON_STROKE} />
                            {m["assets.importToProject"]()}
                          </DropdownMenuItem>
                          <DropdownMenuItem
                            onClick={() => setEditingItem({ id: item.id, name: item.name })}
                          >
                            {m["assets.rename"]()}
                          </DropdownMenuItem>
                          <DropdownMenuItem
                            onClick={() =>
                              setTaggingItem({ id: item.id, tags: item.tags.join(", ") })
                            }
                          >
                            {m["assets.editTags"]()}
                          </DropdownMenuItem>
                          <DropdownMenuSeparator />
                          <DropdownMenuItem
                            className="text-danger"
                            onClick={() => void handleRemoveItems([item.id])}
                          >
                            <Trash2 className="mr-2 size-3.5" strokeWidth={ICON_STROKE} />
                            {m["assets.remove"]()}
                          </DropdownMenuItem>
                        </DropdownMenuContent>
                      </DropdownMenu>
                    </div>
                  </div>

                  {/* Card bottom info */}
                  <div className="flex flex-1 flex-col p-2.5">
                    <span className="truncate text-xs font-medium text-fg" title={item.name}>
                      {item.name}
                    </span>
                    <div className="mt-1 flex items-center justify-between text-[11px] text-fg3">
                      <span>{formatBytes(item.size)}</span>
                      <span>{friendlyMediaTypeLabel(item.media_type)}</span>
                    </div>
                    {item.tags.length > 0 && (
                      <div className="mt-1.5 flex flex-wrap gap-1 overflow-hidden">
                        {item.tags.slice(0, 3).map((t) => (
                          <span
                            key={t}
                            className="rounded bg-accent-tint px-1 py-0.5 text-[10px] text-accent truncate max-w-[80px]"
                          >
                            #{t}
                          </span>
                        ))}
                      </div>
                    )}
                  </div>
                </div>
              );
            })}
          </div>
        ) : (
          /* List View */
          <div className="overflow-x-auto">
            <table
              className="w-full min-w-[700px] table-fixed border-separate border-spacing-0 text-ui"
              data-testid="assets-table"
            >
              <thead>
                <tr className="text-left text-caption text-fg2">
                  <th scope="col" className="w-8 border-b border-hair px-2 pb-2">
                    <input
                      type="checkbox"
                      checked={visibleItems.length > 0 && selectedIds.size === visibleItems.length}
                      onChange={(e) => (e.target.checked ? selectAll() : deselectAll())}
                      aria-label={m["assets.selectAll"]()}
                    />
                  </th>
                  <th scope="col" className="w-[35%] border-b border-hair px-3 pb-2 font-normal">
                    {m["assets.columnName"]()}
                  </th>
                  <th scope="col" className="w-[15%] border-b border-hair px-3 pb-2 font-normal">
                    {m["assets.columnType"]()}
                  </th>
                  <th scope="col" className="w-[12%] border-b border-hair px-3 pb-2 font-normal">
                    {m["assets.columnSize"]()}
                  </th>
                  <th scope="col" className="border-b border-hair px-3 pb-2 font-normal">
                    {m["assets.columnTags"]()}
                  </th>
                  <th scope="col" className="w-[15%] border-b border-hair px-3 pb-2 font-normal">
                    {m["assets.columnAdded"]()}
                  </th>
                  <th scope="col" className="w-16 border-b border-hair px-3 pb-2 text-right">
                    <span className="sr-only">{m["library.actions"]()}</span>
                  </th>
                </tr>
              </thead>
              <tbody>
                {visibleItems.map((item) => {
                  const selected = selectedIds.has(item.id);
                  const group = getMediaTypeGroup(item.media_type);
                  const Icon = MEDIA_ICONS[group] ?? FileBox;
                  const addedTime = formatRelativeTime(item.added_at, getLocale());

                  return (
                    <tr
                      key={item.id}
                      className={cn(
                        "group transition-colors hover:bg-hover",
                        selected && "bg-active",
                      )}
                      data-testid="asset-row"
                      data-asset-id={item.id}
                    >
                      <td className="h-11 border-b border-hair-soft px-2">
                        <input
                          type="checkbox"
                          checked={selected}
                          onChange={() => toggleSelect(item.id)}
                          aria-label={`Select ${item.name}`}
                        />
                      </td>
                      <td className="h-11 truncate border-b border-hair-soft px-3 font-medium text-fg">
                        <span className="flex items-center gap-2">
                          <Icon className="size-4 shrink-0 text-fg3" strokeWidth={ICON_STROKE} />
                          <span className="truncate" title={item.name}>
                            {item.name}
                          </span>
                        </span>
                      </td>
                      <td className="h-11 truncate border-b border-hair-soft px-3 text-fg2">
                        {friendlyMediaTypeLabel(item.media_type)}
                      </td>
                      <td className="h-11 truncate border-b border-hair-soft px-3 text-fg2">
                        {formatBytes(item.size)}
                      </td>
                      <td className="h-11 truncate border-b border-hair-soft px-3 text-fg2">
                        {item.tags.length > 0 ? item.tags.map((t) => `#${t}`).join(" ") : "—"}
                      </td>
                      <td className="h-11 truncate border-b border-hair-soft px-3 text-fg3 text-xs">
                        {addedTime}
                      </td>
                      <td className="h-11 border-b border-hair-soft px-3 text-right">
                        <DropdownMenu>
                          <DropdownMenuTrigger asChild>
                            <button
                              type="button"
                              aria-label={m["common.moreOptions"]()}
                              className="size-7 rounded p-1 text-fg3 hover:bg-hover hover:text-fg focus-visible:outline-none"
                            >
                              <MoreHorizontal className="size-4" strokeWidth={ICON_STROKE} />
                            </button>
                          </DropdownMenuTrigger>
                          <DropdownMenuContent align="end" className="w-44">
                            <DropdownMenuItem onClick={() => void handleImportSelected([item])}>
                              <FolderPlus className="mr-2 size-3.5" strokeWidth={ICON_STROKE} />
                              {m["assets.importToProject"]()}
                            </DropdownMenuItem>
                            <DropdownMenuItem
                              onClick={() => setEditingItem({ id: item.id, name: item.name })}
                            >
                              {m["assets.rename"]()}
                            </DropdownMenuItem>
                            <DropdownMenuItem
                              onClick={() =>
                                setTaggingItem({ id: item.id, tags: item.tags.join(", ") })
                              }
                            >
                              {m["assets.editTags"]()}
                            </DropdownMenuItem>
                            <DropdownMenuSeparator />
                            <DropdownMenuItem
                              className="text-danger"
                              onClick={() => void handleRemoveItems([item.id])}
                            >
                              <Trash2 className="mr-2 size-3.5" strokeWidth={ICON_STROKE} />
                              {m["assets.remove"]()}
                            </DropdownMenuItem>
                          </DropdownMenuContent>
                        </DropdownMenu>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
      </div>

      {/* Rename dialog */}
      {editingItem && (
        <Dialog open={Boolean(editingItem)} onOpenChange={(open) => !open && setEditingItem(null)}>
          <DialogContent className="max-w-sm">
            <DialogHeader>
              <DialogTitle>{m["assets.renamePrompt"]()}</DialogTitle>
            </DialogHeader>
            <div className="py-2">
              <Input
                value={editingItem.name}
                onChange={(e) => setEditingItem({ ...editingItem, name: e.target.value })}
                onKeyDown={(e) => e.key === "Enter" && void handleRenameSave()}
                autoFocus
                className="text-ui"
                data-testid="rename-input"
              />
            </div>
            <DialogFooter>
              <Button variant="ghost" size="sm" onClick={() => setEditingItem(null)}>
                {m["common.cancel"]()}
              </Button>
              <Button
                size="sm"
                onClick={() => void handleRenameSave()}
                data-testid="rename-save-button"
              >
                {m["common.save"]()}
              </Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>
      )}

      {/* Edit tags dialog */}
      {taggingItem && (
        <Dialog open={Boolean(taggingItem)} onOpenChange={(open) => !open && setTaggingItem(null)}>
          <DialogContent className="max-w-sm">
            <DialogHeader>
              <DialogTitle>{m["assets.editTags"]()}</DialogTitle>
              <DialogDescription>{m["assets.tagsPrompt"]()}</DialogDescription>
            </DialogHeader>
            <div className="py-2">
              <Input
                value={taggingItem.tags}
                onChange={(e) => setTaggingItem({ ...taggingItem, tags: e.target.value })}
                onKeyDown={(e) => e.key === "Enter" && void handleTagsSave()}
                placeholder="ui, reference, textures"
                autoFocus
                className="text-ui"
                data-testid="tags-input"
              />
            </div>
            <DialogFooter>
              <Button variant="ghost" size="sm" onClick={() => setTaggingItem(null)}>
                {m["common.cancel"]()}
              </Button>
              <Button
                size="sm"
                onClick={() => void handleTagsSave()}
                data-testid="tags-save-button"
              >
                {m["common.save"]()}
              </Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>
      )}

      {/* Purge removed items confirmation dialog */}
      <Dialog
        open={purgeDialogOpen}
        onOpenChange={(next) => !next && !purging && setPurgeDialogOpen(false)}
      >
        <DialogContent
          showCloseButton={!purging}
          data-testid="assets-purge-dialog"
          className="sm:max-w-md"
        >
          <DialogHeader>
            <DialogTitle>{m["assets.purgeRemovedConfirmTitle"]()}</DialogTitle>
            <DialogDescription>{m["assets.purgeRemovedConfirmDesc"]()}</DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              disabled={purging}
              onClick={() => setPurgeDialogOpen(false)}
            >
              {m["common.cancel"]()}
            </Button>
            <Button
              type="button"
              variant="destructive"
              disabled={purging}
              onClick={() => void handlePurgeRemoved()}
              data-testid="assets-purge-confirm-btn"
            >
              {purging ? m["storage.purging"]() : m["storage.purgeConfirmButton"]()}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Pre-write storage budget confirmation */}
      <StoragePreflightDialog
        pending={pendingPreflight}
        busy={addingBusy}
        onCancel={() => {
          setPendingAddPaths(null);
          setPendingPreflight(null);
        }}
        onConfirm={handleConfirmPreflightAdd}
      />
    </div>
  );
}
