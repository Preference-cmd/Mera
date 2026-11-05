import { useEffect, useRef, useState, type CSSProperties } from "react";
import { useReactFlow } from "@xyflow/react";
import { FileBox, FileText, Film, Images, Music, Search, X } from "lucide-react";
import { toast } from "sonner";
import * as m from "$paraglide/messages";
import { ICON_STROKE } from "@/design/tokens";
import { formatBytes } from "@/lib/library";
import { getMediaTypeGroup } from "@/lib/globalLibrary";
import { useGlobalLibrary } from "@/hooks/useGlobalLibrary";
import { useProjectStore } from "@/store/project";
import { useUIStore } from "@/store/uiStore";
import { getLibraryAssetUrl } from "@/ipc";
import type { LibraryItem } from "@/ipc/schemas";

const MEDIA_ICONS: Record<string, typeof FileBox> = {
  image: Images,
  text: FileText,
  video: Film,
  audio: Music,
  other: FileBox,
};

type DragState = {
  item: LibraryItem;
  x: number;
  y: number;
};

export function CanvasLibraryPanel() {
  const open = useUIStore((state) => state.canvasLibraryPanelOpen);
  if (!open) return null;
  return <CanvasLibraryPanelContent />;
}

function CanvasLibraryPanelContent() {
  const setOpen = useUIStore((state) => state.setCanvasLibraryPanelOpen);
  const projectId = useProjectStore((state) => state.activeProjectId);
  const { items, isLoading, importToProject } = useGlobalLibrary(projectId);
  const { screenToFlowPosition } = useReactFlow();

  const [search, setSearch] = useState("");
  const [dragState, setDragState] = useState<DragState | null>(null);
  const dragStateRef = useRef<DragState | null>(null);
  dragStateRef.current = dragState;

  const filteredItems = items.filter((item) =>
    search.trim() ? item.name.toLowerCase().includes(search.trim().toLowerCase()) : true,
  );

  // Global pointer move, pointer up, and Escape key listeners during drag
  useEffect(() => {
    if (!dragState) return;

    const handlePointerMove = (e: PointerEvent) => {
      setDragState((prev) => (prev ? { ...prev, x: e.clientX, y: e.clientY } : null));
    };

    const handlePointerUp = (e: PointerEvent) => {
      const current = dragStateRef.current;
      setDragState(null);
      if (!current) return;

      // Check if dropped onto the canvas pane or wrapper
      const target = document.elementFromPoint(e.clientX, e.clientY);
      const isCanvas = Boolean(target?.closest(".react-flow__pane, [data-testid='board-canvas']"));
      if (!isCanvas) return;

      const flowPoint = screenToFlowPosition({ x: e.clientX, y: e.clientY });
      void (async () => {
        try {
          await importToProject([current.item], flowPoint);
          toast.success(m["assets.importSuccess"]());
        } catch (err) {
          toast.error(err instanceof Error ? err.message : m["assets.importFailed"]());
        }
      })();
    };

    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        setDragState(null);
      }
    };

    window.addEventListener("pointermove", handlePointerMove);
    window.addEventListener("pointerup", handlePointerUp);
    window.addEventListener("keydown", handleKeyDown);

    return () => {
      window.removeEventListener("pointermove", handlePointerMove);
      window.removeEventListener("pointerup", handlePointerUp);
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [dragState, screenToFlowPosition, importToProject]);

  if (!open) return null;

  return (
    <>
      <div
        className="absolute top-4 left-4 z-20 flex w-72 flex-col overflow-hidden rounded-xl border border-hair-strong bg-surface/95 shadow-2 backdrop-blur-md"
        data-testid="canvas-library-panel"
        onDoubleClick={(e) => e.stopPropagation()}
      >
        {/* Header */}
        <div className="flex items-center justify-between border-b border-hair px-3 py-2.5">
          <div className="flex items-center gap-1.5 font-medium text-fg text-body-sm">
            <Images className="size-4 text-accent" strokeWidth={ICON_STROKE} />
            <span>{m["board.toolAssetLibrary"]()}</span>
          </div>
          <button
            type="button"
            onClick={() => setOpen(false)}
            aria-label="Close"
            className="rounded p-1 text-fg3 hover:bg-hover hover:text-fg"
          >
            <X className="size-3.5" strokeWidth={ICON_STROKE} />
          </button>
        </div>

        {/* Search */}
        <div className="border-b border-hair p-2">
          <label className="flex h-7 items-center gap-1.5 rounded-md bg-tile px-2 text-fg3 shadow-[inset_0_0_0_1px_var(--color-hair)] focus-within:shadow-[inset_0_0_0_1px_var(--color-accent-line)]">
            <Search className="size-3 shrink-0" strokeWidth={ICON_STROKE} />
            <input
              type="text"
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              placeholder={m["assets.searchPlaceholder"]()}
              className="min-w-0 flex-1 border-0 bg-transparent text-xs text-fg outline-none placeholder:text-fg3"
            />
          </label>
        </div>

        {/* Items list */}
        <div className="max-h-80 min-h-48 overflow-y-auto p-2 space-y-1.5">
          {isLoading ? (
            <div className="py-8 text-center text-xs text-fg3">Loading…</div>
          ) : filteredItems.length === 0 ? (
            <div className="py-8 text-center text-xs text-fg3">{m["assets.empty"]()}</div>
          ) : (
            filteredItems.map((item) => {
              const group = getMediaTypeGroup(item.media_type);
              const Icon = MEDIA_ICONS[group] ?? FileBox;

              return (
                <div
                  key={item.id}
                  onPointerDown={(e) => {
                    // Left mouse button only
                    if (e.button !== 0) return;
                    e.preventDefault();
                    setDragState({
                      item,
                      x: e.clientX,
                      y: e.clientY,
                    });
                  }}
                  className="group flex cursor-grab active:cursor-grabbing items-center gap-2.5 rounded-lg border border-hair bg-tile p-1.5 text-ui transition-colors hover:border-hair-strong hover:bg-hover"
                  data-testid="canvas-library-item"
                  data-item-id={item.id}
                >
                  <div className="relative size-10 shrink-0 overflow-hidden rounded bg-tile-subtle flex items-center justify-center">
                    {group === "image" ? (
                      <img
                        src={getLibraryAssetUrl(item.id)}
                        alt={item.name}
                        className="h-full w-full object-cover"
                      />
                    ) : (
                      <Icon className="size-5 text-fg3" strokeWidth={ICON_STROKE} />
                    )}
                  </div>
                  <div className="min-w-0 flex-1">
                    <p className="truncate text-xs font-medium text-fg" title={item.name}>
                      {item.name}
                    </p>
                    <p className="text-[10px] text-fg3">{formatBytes(item.size)}</p>
                  </div>
                </div>
              );
            })
          )}
        </div>

        {/* Footer hint */}
        <div className="border-t border-hair px-3 py-1.5 text-center text-[10px] text-fg3">
          {m["board.dragToBoard"]()}
        </div>
      </div>

      {/* Floating Drag Ghost */}
      {dragState && (
        <div
          className="pointer-events-none fixed z-50 flex items-center gap-2 rounded-lg border border-accent bg-surface/95 px-3 py-2 shadow-2 backdrop-blur-sm -translate-x-1/2 -translate-y-1/2"
          style={
            {
              left: `${dragState.x}px`,
              top: `${dragState.y}px`,
            } as CSSProperties
          }
          data-testid="drag-ghost"
        >
          <Images className="size-4 text-accent" strokeWidth={ICON_STROKE} />
          <span className="text-xs font-medium text-fg max-w-[140px] truncate">
            {dragState.item.name}
          </span>
        </div>
      )}
    </>
  );
}
