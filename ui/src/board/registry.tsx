import { useEffect, useRef, useState } from "react";
import { Boxes, FileBox, FileImage, FileText, Film, Layers, Lock, Music, Play } from "lucide-react";
import { useNavigate } from "@tanstack/react-router";
import * as m from "$paraglide/messages";
import { ICON_STROKE } from "@/design/tokens";
import { useProjectStore } from "@/store/project";
import { useCollection } from "@/hooks/useCollection";
import { getAssetUrl, readAssetPreview, type AssetPreview } from "@/ipc";
import { listAssets, listDynamicWorkflows, listRuns } from "@/lib/library";
import { friendlyMediaTypeLabel, getMediaTypeGroup } from "@/lib/globalLibrary";
import { ImageThumbnail } from "@/components/ui/ImageThumbnail";
import type { AssetSummary, BoardItemKind } from "@/ipc/schemas";

const MEDIA_ICONS = {
  image: FileImage,
  text: FileText,
  video: Film,
  audio: Music,
  other: FileBox,
} as const;

export type BoardCardProps = {
  id: string;
  kind: BoardItemKind | { type: string; [key: string]: unknown };
  size: { width: number; height: number };
  locked: boolean;
  selected?: boolean;
  isEditing?: boolean;
  onStartEdit?: () => void;
  onEndEdit?: () => void;
  onUpdateNote?: (content: string) => void;
};

export function NoteCard({
  kind,
  locked,
  isEditing,
  onStartEdit,
  onEndEdit,
  onUpdateNote,
}: BoardCardProps) {
  const content = (kind as { type: "note"; content: string }).content ?? "";
  const [draft, setDraft] = useState(content);
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);
  const sessionEndedRef = useRef(false);

  useEffect(() => {
    setDraft(content);
  }, [content]);

  const isEditingActive = Boolean(isEditing && !locked);

  useEffect(() => {
    if (isEditingActive) {
      sessionEndedRef.current = false;
      setDraft(content);
      const timer = setTimeout(() => {
        if (textareaRef.current) {
          textareaRef.current.focus();
          textareaRef.current.selectionStart = textareaRef.current.value.length;
          textareaRef.current.selectionEnd = textareaRef.current.value.length;
        }
      }, 0);
      return () => clearTimeout(timer);
    }
  }, [isEditingActive, content]);

  function handleBlur() {
    if (!isEditingActive || sessionEndedRef.current) return;
    sessionEndedRef.current = true;
    if (draft !== content) {
      onUpdateNote?.(draft);
    }
    onEndEdit?.();
  }

  function handleKeyDown(e: React.KeyboardEvent<HTMLTextAreaElement>) {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      sessionEndedRef.current = true;
      setDraft(content);
      onEndEdit?.();
      return;
    }
    if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      e.stopPropagation();
      if (sessionEndedRef.current) return;
      sessionEndedRef.current = true;
      if (draft !== content) {
        onUpdateNote?.(draft);
      }
      onEndEdit?.();
    }
  }

  return (
    <div
      className="flex h-full w-full flex-col overflow-hidden rounded-lg border border-hair-strong bg-note p-4 text-fg shadow-1 transition-shadow select-none hover:shadow-2"
      onDoubleClick={(e) => {
        e.stopPropagation();
        if (locked) return;
        onStartEdit?.();
      }}
      data-testid="note-card"
    >
      <div className="mb-2 flex items-center justify-between text-caption font-medium text-fg2">
        <span>{m["board.note"]()}</span>
        {locked && (
          <span
            title={m["board.locked"]()}
            aria-label={m["board.locked"]()}
            data-testid="lock-indicator"
          >
            <Lock className="h-3.5 w-3.5 text-fg3" aria-hidden="true" strokeWidth={ICON_STROKE} />
          </span>
        )}
      </div>
      {isEditingActive ? (
        <textarea
          ref={textareaRef}
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onBlur={handleBlur}
          onKeyDown={handleKeyDown}
          className="nodrag nopan nowheel h-full w-full resize-none border-0 bg-transparent p-0 font-sans text-body-sm text-fg outline-none focus:ring-0"
          placeholder={m["board.notePlaceholder"]()}
          data-testid="note-textarea"
        />
      ) : (
        <div className="h-full w-full overflow-y-auto whitespace-pre-wrap font-sans text-body-sm leading-relaxed text-fg">
          {content ? (
            content
          ) : (
            <span className="italic text-fg2">{m["board.doubleClickToEdit"]()}</span>
          )}
        </div>
      )}
    </div>
  );
}

function SlotPreviewTile({ projectId, assetId }: { projectId: string; assetId: string }) {
  const [preview, setPreview] = useState<AssetPreview | null>(null);

  useEffect(() => {
    let active = true;
    readAssetPreview(projectId, assetId, 3)
      .then((res) => {
        if (active) setPreview(res);
      })
      .catch(() => {
        if (active) setPreview(null);
      });
    return () => {
      active = false;
    };
  }, [projectId, assetId]);

  const mediaType = preview?.mediaType ?? "";
  const group = mediaType ? getMediaTypeGroup(mediaType) : "other";
  const Icon = MEDIA_ICONS[group];

  return (
    <div className="flex h-full w-full items-center justify-center overflow-hidden rounded bg-tile">
      {group === "image" ? (
        <ImageThumbnail
          src={preview?.url || getAssetUrl(projectId, assetId)}
          alt={assetId}
          icon={Icon}
          className="h-full w-full object-cover"
        />
      ) : preview?.text ? (
        <pre className="h-full w-full overflow-hidden p-1.5 font-mono text-[9px] leading-tight text-fg3 whitespace-pre-wrap">
          {preview.text}
        </pre>
      ) : (
        <Icon className="size-4 text-fg3" strokeWidth={ICON_STROKE} aria-hidden="true" />
      )}
    </div>
  );
}

export function CollectionCard({ kind, locked }: BoardCardProps) {
  const collectionId = (kind as { type: "collection"; collection_id: string }).collection_id;
  const activeProjectId = useProjectStore((s) => s.activeProjectId);
  const navigate = useNavigate();
  const { collection, loading } = useCollection(activeProjectId, collectionId);

  const slots = collection?.slots ?? [];
  const slotCount = slots.length;
  const previewSlots = slots.slice(0, 4);
  const countLabel =
    slotCount === 1
      ? m["collections.assetCountOne"]()
      : m["collections.assetCountMany"]({ count: slotCount });

  return (
    <div
      onDoubleClick={(e) => {
        e.stopPropagation();
        void navigate({ to: "/collections/$id", params: { id: collectionId } });
      }}
      className="flex h-full w-full cursor-pointer flex-col gap-2 overflow-hidden rounded-lg border border-hair bg-surface p-3 text-fg shadow-1 transition-all select-none hover:border-hair-strong hover:bg-tile hover:shadow-2"
      data-testid="collection-card"
    >
      <div className="flex items-center justify-between gap-2">
        <div className="flex items-center gap-2 min-w-0 flex-1">
          <div className="flex h-6 w-6 shrink-0 items-center justify-center rounded-md bg-accent-tint text-accent">
            <Layers className="h-3.5 w-3.5" strokeWidth={ICON_STROKE} />
          </div>
          <h3 className="truncate font-sans text-body-sm font-semibold text-fg">
            {collection?.name ?? (loading ? m["collections.loading"]() : collectionId)}
          </h3>
        </div>
        {locked && (
          <span
            title={m["board.locked"]()}
            aria-label={m["board.locked"]()}
            data-testid="lock-indicator"
          >
            <Lock
              className="h-3.5 w-3.5 text-fg3 shrink-0"
              aria-hidden="true"
              strokeWidth={ICON_STROKE}
            />
          </span>
        )}
      </div>

      <div
        className={
          previewSlots.length <= 1
            ? "grid min-h-0 flex-1 grid-cols-1 gap-1 overflow-hidden rounded-md"
            : "grid min-h-0 flex-1 grid-cols-2 grid-rows-2 gap-1 overflow-hidden rounded-md"
        }
      >
        {previewSlots.length === 0 ? (
          <div className="flex h-full w-full items-center justify-center rounded bg-tile text-fg3">
            <Layers className="size-5" strokeWidth={ICON_STROKE} aria-hidden="true" />
          </div>
        ) : (
          previewSlots.map((slot) => (
            <SlotPreviewTile key={slot.id} projectId={activeProjectId} assetId={slot.asset_id} />
          ))
        )}
      </div>

      <div className="flex items-center justify-between text-caption text-fg2">
        <span>{countLabel}</span>
      </div>
    </div>
  );
}

export function AssetRefCard({ kind, locked }: BoardCardProps) {
  const artifactId = (kind as { type: "asset_ref"; artifact_id: string }).artifact_id;
  const activeProjectId = useProjectStore((s) => s.activeProjectId);
  const [asset, setAsset] = useState<AssetSummary | null>(null);
  const [preview, setPreview] = useState<AssetPreview | null>(null);

  useEffect(() => {
    let active = true;
    listAssets(activeProjectId)
      .then((list) => {
        if (!active) return;
        setAsset(list.find((a) => a.id === artifactId) ?? null);
      })
      .catch(() => {
        if (active) setAsset(null);
      });
    readAssetPreview(activeProjectId, artifactId, 5)
      .then((res) => {
        if (active) setPreview(res);
      })
      .catch(() => {
        if (active) setPreview(null);
      });
    return () => {
      active = false;
    };
  }, [activeProjectId, artifactId]);

  const mediaType = asset?.media_type ?? preview?.mediaType ?? "";
  const group = mediaType ? getMediaTypeGroup(mediaType) : "other";
  const Icon = MEDIA_ICONS[group];
  const displayName = asset?.name ?? artifactId;

  return (
    <div
      className="flex h-full w-full flex-col gap-2 overflow-hidden rounded-lg border border-hair bg-surface p-3 text-fg shadow-1 transition-shadow select-none hover:shadow-2"
      data-testid="asset-ref-card"
    >
      <div className="relative flex min-h-0 flex-1 items-center justify-center overflow-hidden rounded-md bg-tile">
        {group === "image" ? (
          <ImageThumbnail
            src={preview?.url || getAssetUrl(activeProjectId, artifactId)}
            alt={displayName}
            icon={Icon}
            className="h-full w-full object-contain"
          />
        ) : preview?.text ? (
          <pre className="h-full w-full overflow-hidden p-2 font-mono text-[10px] leading-relaxed text-fg2 whitespace-pre-wrap">
            {preview.text}
          </pre>
        ) : (
          <div className="flex flex-col items-center gap-1 text-fg3">
            <Icon className="h-6 w-6" strokeWidth={ICON_STROKE} aria-hidden="true" />
            {mediaType && <span className="text-[11px]">{friendlyMediaTypeLabel(mediaType)}</span>}
          </div>
        )}
        {locked && (
          <span
            title={m["board.locked"]()}
            aria-label={m["board.locked"]()}
            data-testid="lock-indicator"
            className="absolute top-1.5 right-1.5 rounded bg-surface/90 p-1 shadow-1"
          >
            <Lock
              className="h-3.5 w-3.5 text-fg3 shrink-0"
              aria-hidden="true"
              strokeWidth={ICON_STROKE}
            />
          </span>
        )}
      </div>

      <div className="flex items-center gap-1.5 min-w-0">
        <Icon className="size-3.5 shrink-0 text-fg3" strokeWidth={ICON_STROKE} aria-hidden="true" />
        <p
          className="min-w-0 flex-1 truncate font-sans text-caption font-medium text-fg"
          title={displayName}
        >
          {displayName}
        </p>
      </div>
    </div>
  );
}

export function WorkflowRefCard({ kind, locked }: BoardCardProps) {
  const workflowId = (kind as { type: "workflow_ref"; workflow_id: string }).workflow_id;
  const activeProjectId = useProjectStore((s) => s.activeProjectId);
  const navigate = useNavigate();
  const [workflowName, setWorkflowName] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    listDynamicWorkflows(activeProjectId)
      .then((list) => {
        if (!active) return;
        setWorkflowName(list.find((w) => w.id === workflowId)?.name ?? null);
      })
      .catch(() => {
        if (active) setWorkflowName(null);
      });
    return () => {
      active = false;
    };
  }, [activeProjectId, workflowId]);

  return (
    <div
      onDoubleClick={(e) => {
        e.stopPropagation();
        void navigate({ to: "/library", search: { type: "workflow" } });
      }}
      className="flex h-full w-full cursor-pointer flex-col justify-between rounded-lg border border-hair bg-surface p-4 text-fg shadow-1 transition-all select-none hover:border-hair-strong hover:bg-tile hover:shadow-2"
      data-testid="workflow-ref-card"
    >
      <div className="flex items-start justify-between gap-2.5">
        <div className="flex items-center gap-2.5 min-w-0 flex-1">
          <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-tile text-fg2">
            <Boxes className="h-4 w-4" strokeWidth={ICON_STROKE} />
          </div>
          <div className="min-w-0 flex-1">
            <span className="text-caption font-medium text-fg2">{m["board.workflow"]()}</span>
            <p className="truncate font-sans text-body-sm font-medium text-fg">
              {workflowName ?? workflowId}
            </p>
          </div>
        </div>
        {locked && (
          <span
            title={m["board.locked"]()}
            aria-label={m["board.locked"]()}
            data-testid="lock-indicator"
          >
            <Lock
              className="h-3.5 w-3.5 text-fg3 shrink-0"
              aria-hidden="true"
              strokeWidth={ICON_STROKE}
            />
          </span>
        )}
      </div>
    </div>
  );
}

export function RunRefCard({ kind, locked }: BoardCardProps) {
  const runId = (kind as { type: "run_ref"; run_id: string }).run_id;
  const activeProjectId = useProjectStore((s) => s.activeProjectId);
  const navigate = useNavigate();
  const [runTitle, setRunTitle] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    Promise.all([listRuns(activeProjectId), listDynamicWorkflows(activeProjectId)])
      .then(([runs, workflows]) => {
        if (!active) return;
        const run = runs.find((r) => r.id === runId);
        if (!run) {
          setRunTitle(null);
          return;
        }
        const wfName = workflows.find((w) => w.id === run.dynamic_workflow_id)?.name;
        setRunTitle(wfName ? `${wfName} #${run.id.slice(-6)}` : runId);
      })
      .catch(() => {
        if (active) setRunTitle(null);
      });
    return () => {
      active = false;
    };
  }, [activeProjectId, runId]);

  return (
    <div
      onDoubleClick={(e) => {
        e.stopPropagation();
        void navigate({ to: "/library", search: { type: "run" } });
      }}
      className="flex h-full w-full cursor-pointer flex-col justify-between rounded-lg border border-hair bg-surface p-4 text-fg shadow-1 transition-all select-none hover:border-hair-strong hover:bg-tile hover:shadow-2"
      data-testid="run-ref-card"
    >
      <div className="flex items-start justify-between gap-2.5">
        <div className="flex items-center gap-2.5 min-w-0 flex-1">
          <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-ok/10 text-ok">
            <Play className="h-4 w-4" strokeWidth={ICON_STROKE} />
          </div>
          <div className="min-w-0 flex-1">
            <span className="text-caption font-medium text-fg2">{m["board.run"]()}</span>
            <p className="truncate font-sans text-body-sm font-medium text-fg">
              {runTitle ?? runId}
            </p>
          </div>
        </div>
        {locked && (
          <span
            title={m["board.locked"]()}
            aria-label={m["board.locked"]()}
            data-testid="lock-indicator"
          >
            <Lock
              className="h-3.5 w-3.5 text-fg3 shrink-0"
              aria-hidden="true"
              strokeWidth={ICON_STROKE}
            />
          </span>
        )}
      </div>
    </div>
  );
}

export function PlaceholderCard({ kind, id, locked }: BoardCardProps) {
  const typeName = kind?.type ?? "unknown";

  return (
    <div
      className="flex h-full w-full flex-col justify-between rounded-lg border border-dashed border-hair-strong bg-surface/50 p-4 text-fg shadow-1 transition-shadow select-none"
      data-testid="placeholder-card"
    >
      <div className="flex items-start justify-between gap-2">
        <div>
          <span className="rounded bg-tile px-2 py-0.5 font-mono text-caption font-medium text-fg2">
            {typeName}
          </span>
          <p className="mt-2 text-body-sm text-fg2">{m["board.unregisteredItem"]()}</p>
        </div>
        {locked && (
          <span
            title={m["board.locked"]()}
            aria-label={m["board.locked"]()}
            data-testid="lock-indicator"
          >
            <Lock
              className="h-3.5 w-3.5 text-fg3 shrink-0"
              aria-hidden="true"
              strokeWidth={ICON_STROKE}
            />
          </span>
        )}
      </div>
      <div className="mt-2 font-mono text-caption text-fg2 truncate">id: {id}</div>
    </div>
  );
}

const REGISTRY: Record<string, React.ComponentType<BoardCardProps>> = {
  note: NoteCard,
  collection: CollectionCard,
  asset_ref: AssetRefCard,
  workflow_ref: WorkflowRefCard,
  run_ref: RunRefCard,
};

export function getCardRenderer(type: string): React.ComponentType<BoardCardProps> {
  return REGISTRY[type] ?? PlaceholderCard;
}
