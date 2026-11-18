import { NodeResizer, type NodeProps } from "@xyflow/react";
import { useNavigate } from "@tanstack/react-router";
import { ExternalLink, Pencil, Trash2 } from "lucide-react";
import * as m from "$paraglide/messages";
import { ICON_STROKE } from "@/design/tokens";
import { cn } from "@/lib/utils";
import { useUIStore } from "@/store/uiStore";
import { getCardRenderer } from "./registry";
import { MIN_ITEM_WIDTH, MIN_ITEM_HEIGHT } from "./batch";
import { boardNodeTraceClass } from "./trace";
import type { BoardItem } from "@/ipc/schemas";

export type BoardNodeData = {
  item: BoardItem;
  isEditing?: boolean;
  onStartEdit?: (id: string) => void;
  onEndEdit?: (id: string) => void;
  onUpdateNote?: (id: string, content: string) => void;
  onResizeEnd?: (id: string, size: { width: number; height: number }) => void;
  onDeleteItem?: (id: string) => void;
};

export function BoardNode({ id, data, selected }: NodeProps) {
  const nodeData = data as unknown as BoardNodeData;
  const item = nodeData.item;
  const CardComponent = getCardRenderer(item.kind.type);
  const canvasTrace = useUIStore((state) => state.canvasTrace);
  const navigate = useNavigate();

  const canOpen = item.kind.type === "collection" || item.kind.type === "run_ref";
  const canEdit = item.kind.type === "note" && !item.locked;
  const canDelete = !item.locked && Boolean(nodeData.onDeleteItem);
  const showActions = Boolean(selected && !nodeData.isEditing && (canOpen || canEdit || canDelete));

  function handleOpen() {
    if (item.kind.type === "collection") {
      const colId = (item.kind as { type: "collection"; collection_id: string }).collection_id;
      void navigate({ to: "/collections/$id", params: { id: colId } });
    } else if (item.kind.type === "run_ref") {
      void navigate({ to: "/library", search: { type: "run" } });
    }
  }

  return (
    <div
      className={cn(
        "relative h-full w-full transition-[opacity,box-shadow] duration-200 ease-ui motion-reduce:transition-none",
        boardNodeTraceClass(canvasTrace, id),
      )}
      data-tracing={canvasTrace ? String(canvasTrace.targets.includes(id)) : undefined}
      data-testid="board-node"
    >
      {showActions && (
        <div
          role="toolbar"
          aria-label={m["library.actions"]()}
          data-testid="node-selection-toolbar"
          onPointerDown={(e) => e.stopPropagation()}
          onDoubleClick={(e) => e.stopPropagation()}
          className="nodrag nopan absolute -top-9 right-0 z-30 flex items-center gap-0.5 rounded-lg border border-hair-strong bg-surface p-1 shadow-2"
        >
          {canEdit && (
            <button
              type="button"
              onClick={() => nodeData.onStartEdit?.(id)}
              aria-label={m["board.editNote"]()}
              title={m["board.editNote"]()}
              className="flex h-6 cursor-pointer items-center gap-1 rounded px-2 text-caption font-medium text-fg2 transition-colors hover:bg-hover hover:text-fg focus-visible:ring-2 focus-visible:ring-accent focus-visible:outline-none"
            >
              <Pencil className="size-3" strokeWidth={ICON_STROKE} aria-hidden="true" />
              {m["board.editNote"]()}
            </button>
          )}
          {canOpen && (
            <button
              type="button"
              onClick={handleOpen}
              aria-label={m["board.openDetail"]()}
              title={m["board.openDetail"]()}
              className="flex h-6 cursor-pointer items-center gap-1 rounded px-2 text-caption font-medium text-fg2 transition-colors hover:bg-hover hover:text-fg focus-visible:ring-2 focus-visible:ring-accent focus-visible:outline-none"
            >
              <ExternalLink className="size-3" strokeWidth={ICON_STROKE} aria-hidden="true" />
              {m["board.openDetail"]()}
            </button>
          )}
          {canDelete && (
            <button
              type="button"
              onClick={() => nodeData.onDeleteItem?.(id)}
              aria-label={m["board.deleteItem"]()}
              title={m["board.deleteItem"]()}
              className="flex h-6 cursor-pointer items-center gap-1 rounded px-2 text-caption font-medium text-fg2 transition-colors hover:bg-err/10 hover:text-err focus-visible:ring-2 focus-visible:ring-accent focus-visible:outline-none"
            >
              <Trash2 className="size-3" strokeWidth={ICON_STROKE} aria-hidden="true" />
              {m["board.deleteItem"]()}
            </button>
          )}
        </div>
      )}
      <NodeResizer
        minWidth={MIN_ITEM_WIDTH}
        minHeight={MIN_ITEM_HEIGHT}
        isVisible={Boolean(selected && !item.locked)}
        lineClassName="!border-accent"
        handleClassName="!h-2.5 !w-2.5 !rounded-xs !border-surface !bg-accent"
        onResizeEnd={(_event, params) => {
          nodeData.onResizeEnd?.(id, { width: params.width, height: params.height });
        }}
      />
      <CardComponent
        id={item.id}
        kind={item.kind}
        size={item.size}
        locked={item.locked}
        selected={selected}
        isEditing={nodeData.isEditing}
        onStartEdit={() => nodeData.onStartEdit?.(id)}
        onEndEdit={() => nodeData.onEndEdit?.(id)}
        onUpdateNote={(content) => nodeData.onUpdateNote?.(id, content)}
      />
    </div>
  );
}
