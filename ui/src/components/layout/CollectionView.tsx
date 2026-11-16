import { useCallback, useEffect, useRef, useState } from "react";
import { ArrowLeft, CircleAlert, Redo2, Undo2 } from "lucide-react";
import { useNavigate, useParams, useRouter } from "@tanstack/react-router";
import * as m from "$paraglide/messages";
import { ICON_STROKE } from "@/design/tokens";
import { useProjectStore } from "@/store/project";
import { isShellModalOpen } from "@/store/uiStore";
import { opInvoke, readAssetPreview, type AssetPreview } from "@/ipc";
import {
  type CollectionSlot,
  type OperationInfo,
  type RecipeStep,
  type SlotRunState,
} from "@/ipc/schemas";
import { useCollection } from "@/hooks/useCollection";
import { useCollectionRun } from "@/hooks/useCollectionRun";
import { useOperations } from "@/hooks/useOperations";
import { FocusToolbar } from "@/components/layout/FocusToolbar";
import { SlotRecipePanel } from "@/components/layout/SlotRecipePanel";
import { TextToolDialog } from "@/components/layout/TextToolDialog";

const PREVIEW_LINES = 4;

type ToolDialogState =
  | {
      mode: "append";
      slot: CollectionSlot;
      operation: OperationInfo;
    }
  | {
      mode: "edit";
      slot: CollectionSlot;
      operation: OperationInfo;
      step: RecipeStep;
      rootAssetId: string;
    };

function slotStateLabel(state: SlotRunState): string {
  switch (state) {
    case "queued":
      return m["collections.slotState.queued"]();
    case "running":
      return m["collections.slotState.running"]();
    case "succeeded":
      return m["collections.slotState.succeeded"]();
    case "failed":
      return m["collections.slotState.failed"]();
    case "stale":
      return m["collections.slotState.stale"]();
    case "cancelled":
      return m["collections.slotState.cancelled"]();
    case "needs_confirmation":
      return m["collections.slotState.needsConfirmation"]();
  }
}

function slotStateClass(state: SlotRunState): string {
  switch (state) {
    case "running":
      return "bg-info/10 text-fg";
    case "succeeded":
      return "bg-ok/10 text-fg";
    case "failed":
      return "bg-err/10 text-fg";
    case "needs_confirmation":
      return "bg-warn/10 text-fg";
    default:
      return "bg-tile text-fg2";
  }
}

/** The tinted pill keeps the text at `fg`; the dot alone carries the state colour. */
function slotStateDotClass(state: SlotRunState): string | null {
  switch (state) {
    case "running":
      return "bg-info";
    case "succeeded":
      return "bg-ok";
    case "failed":
      return "bg-err";
    case "needs_confirmation":
      return "bg-warn";
    default:
      return null;
  }
}

function SlotItem({
  projectId,
  slot,
  state,
  focused,
  onFocus,
  onMediaType,
}: {
  projectId: string;
  slot: CollectionSlot;
  state?: SlotRunState;
  focused: boolean;
  onFocus: () => void;
  onMediaType: (slotId: string, mediaType: string) => void;
}) {
  const [preview, setPreview] = useState<AssetPreview | null>(null);
  const [slotError, setSlotError] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    setPreview(null);
    setSlotError(null);
    readAssetPreview(projectId, slot.asset_id, PREVIEW_LINES).then(
      (result) => {
        if (!active) return;
        setPreview(result);
        onMediaType(slot.id, result.mediaType);
      },
      (err: unknown) => {
        if (!active) return;
        setSlotError(err instanceof Error ? err.message : String(err));
        onMediaType(slot.id, "");
      },
    );
    return () => {
      active = false;
    };
  }, [projectId, slot.id, slot.asset_id, onMediaType]);

  const mediaType = preview?.mediaType ?? "";
  const showStaleBadge = slot.stale === true && state !== "stale";

  return (
    <button
      type="button"
      data-testid={`slot-card-${slot.id}`}
      aria-pressed={focused}
      onClick={onFocus}
      className={`flex cursor-pointer flex-col rounded-lg border bg-surface p-4 text-left shadow-1 transition-all focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/30 ${
        focused ? "border-accent ring-2 ring-accent/25" : "border-hair hover:border-hair-strong"
      }`}
    >
      <div className="flex w-full items-center justify-between border-b border-hair pb-2">
        <span className="font-mono text-caption font-semibold text-accent">
          {m["collections.slotPrefix"]()} {slot.id}
        </span>
        <span className="flex items-center gap-2">
          {showStaleBadge && (
            <span className="rounded bg-warn/10 px-2 py-0.5 font-mono text-caption text-warn">
              {m["collections.slotState.stale"]()}
            </span>
          )}
          {state && (
            <span
              className={`flex items-center gap-1.5 rounded px-2 py-0.5 font-mono text-caption ${slotStateClass(state)}`}
            >
              {slotStateDotClass(state) && (
                <span
                  aria-hidden="true"
                  className={`size-1.5 shrink-0 rounded-full ${slotStateDotClass(state)}`}
                />
              )}
              {slotStateLabel(state)}
            </span>
          )}
          <span className="rounded bg-tile px-2 py-0.5 font-mono text-caption text-fg2">
            {mediaType}
          </span>
        </span>
      </div>

      <div className="mt-2 flex w-full items-center justify-between gap-2 text-caption text-fg2">
        <span>
          {m["collections.assetIdPrefix"]()} <span className="font-mono">{slot.asset_id}</span>
        </span>
      </div>

      {slotError ? (
        <div className="mt-3 w-full flex-1 rounded-lg border-l-2 border-err bg-err/10 p-3 font-mono text-caption text-fg">
          {slotError}
        </div>
      ) : (
        <div className="mt-3 w-full flex-1 rounded-lg bg-tile p-3 font-mono text-caption text-fg">
          {preview?.text ? (
            <pre className="whitespace-pre-wrap">{preview.text}</pre>
          ) : preview && mediaType.startsWith("image/") ? (
            <img src={preview.url} alt={slot.id} className="max-h-48 rounded object-contain" />
          ) : (
            <span className="italic text-fg2">{m["collections.noPreview"]()}</span>
          )}
        </div>
      )}
    </button>
  );
}

export function CollectionView() {
  const { id } = useParams({ strict: false }) as { id: string };
  const activeProjectId = useProjectStore((s) => s.activeProjectId);
  const navigate = useNavigate();
  const router = useRouter();

  const {
    collection,
    loading,
    error: collectionError,
    refresh: fetchCollection,
  } = useCollection(activeProjectId, id);
  const { slotStates } = useCollectionRun(activeProjectId, id);
  const {
    operations,
    loading: operationsLoading,
    textOperations,
    error: textToolError,
    retry: retryTextTools,
  } = useOperations(activeProjectId);
  const [actionError, setActionError] = useState<string | null>(null);
  const [focusedSlotId, setFocusedSlotId] = useState<string | null>(null);
  const [slotMediaTypes, setSlotMediaTypes] = useState<Record<string, string>>({});
  const [dialogState, setDialogState] = useState<ToolDialogState | null>(null);
  const error = collectionError ?? actionError;

  const handleMediaType = useCallback((slotId: string, mediaType: string) => {
    setSlotMediaTypes((current) =>
      current[slotId] === mediaType ? current : { ...current, [slotId]: mediaType },
    );
  }, []);

  // Keep focus if the slot still exists after collection refresh; clear otherwise.
  useEffect(() => {
    if (!collection || focusedSlotId === null) return;
    if (!collection.slots.some((slot) => slot.id === focusedSlotId)) {
      setFocusedSlotId(null);
    }
  }, [collection, focusedSlotId]);

  // On project switch: reset state and navigate back to the library
  const prevProjectIdRef = useRef(activeProjectId);
  useEffect(() => {
    if (prevProjectIdRef.current !== activeProjectId) {
      prevProjectIdRef.current = activeProjectId;
      setActionError(null);
      setFocusedSlotId(null);
      setDialogState(null);
      void navigate({ to: "/library" });
    }
  }, [activeProjectId, navigate]);

  const handleUndo = useCallback(async () => {
    if (!id) return;
    try {
      setActionError(null);
      await opInvoke(activeProjectId, "collection.undo", { collection_id: id });
      await fetchCollection();
    } catch (e) {
      setActionError(e instanceof Error ? e.message : String(e));
    }
  }, [id, activeProjectId, fetchCollection]);

  const handleRedo = useCallback(async () => {
    if (!id) return;
    try {
      setActionError(null);
      await opInvoke(activeProjectId, "collection.redo", { collection_id: id });
      await fetchCollection();
    } catch (e) {
      setActionError(e instanceof Error ? e.message : String(e));
    }
  }, [id, activeProjectId, fetchCollection]);

  // Escape unfocuses the active slot; Cmd/Ctrl+Z and Cmd/Ctrl+Shift+Z / Ctrl+Y drive collection undo/redo.
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.defaultPrevented || e.repeat) return;

      if (e.key === "Escape") {
        if (!dialogState && !isShellModalOpen() && focusedSlotId !== null) {
          setFocusedSlotId(null);
        }
        return;
      }

      if (e.altKey) return;
      const mod = e.metaKey || e.ctrlKey;
      if (!mod) return;
      if (isShellModalOpen() || dialogState !== null) return;

      const target = e.target as HTMLElement | null;
      if (
        target &&
        (target.tagName === "INPUT" ||
          target.tagName === "TEXTAREA" ||
          target.tagName === "SELECT" ||
          target.isContentEditable)
      ) {
        return;
      }

      const key = e.key.toLowerCase();
      if (key === "z" && !e.shiftKey) {
        e.preventDefault();
        void handleUndo();
      } else if ((key === "z" && e.shiftKey) || (key === "y" && !e.shiftKey && e.ctrlKey)) {
        e.preventDefault();
        void handleRedo();
      }
    };

    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [dialogState, focusedSlotId, handleUndo, handleRedo]);

  // Return to whichever mode opened the detail; a direct link has no history
  // entry, so it falls back to the library.
  function handleBack() {
    if (router.history.canGoBack()) {
      router.history.back();
      return;
    }
    void navigate({ to: "/library" });
  }

  const focusedSlot =
    focusedSlotId && collection
      ? (collection.slots.find((slot) => slot.id === focusedSlotId) ?? null)
      : null;
  const focusedMediaType = focusedSlot ? (slotMediaTypes[focusedSlot.id] ?? "") : "";
  const showFocusToolbar = focusedSlot !== null && focusedMediaType.startsWith("text/");

  return (
    <div className="flex h-full flex-col overflow-y-auto bg-ground p-6">
      {/* Top back navigation */}
      <div className="mb-4">
        <button
          type="button"
          onClick={handleBack}
          className="inline-flex cursor-pointer items-center gap-1.5 rounded-sm text-caption font-medium text-fg2 transition-colors hover:text-fg focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/30"
        >
          <ArrowLeft className="h-3.5 w-3.5" strokeWidth={ICON_STROKE} />
          {m["common.back"]()}
        </button>
      </div>

      {/* Header */}
      <div className="mb-6 flex flex-wrap items-center justify-between gap-4">
        <div>
          <h1 className="text-display-sm font-semibold text-fg">
            {collection?.name ?? m["collections.defaultTitle"]()}
          </h1>
          <p className="mt-1 text-body-sm text-fg2">
            {collection
              ? `${collection.slots.length} ${
                  collection.slots.length === 1 ? m["collections.slot"]() : m["collections.slots"]()
                } • ${m["collections.version"]()} ${collection.version}`
              : m["collections.loading"]()}
          </p>
        </div>

        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={() => void handleUndo()}
            title={m["collections.undo"]()}
            className="flex h-9 cursor-pointer items-center gap-1.5 rounded-lg border border-hair bg-surface px-3 text-body-sm font-medium text-fg transition-colors hover:bg-hover focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/30"
          >
            <Undo2 className="h-4 w-4" strokeWidth={ICON_STROKE} />
            {m["collections.undo"]()}
          </button>
          <button
            type="button"
            onClick={() => void handleRedo()}
            title={m["collections.redo"]()}
            className="flex h-9 cursor-pointer items-center gap-1.5 rounded-lg border border-hair bg-surface px-3 text-body-sm font-medium text-fg transition-colors hover:bg-hover focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/30"
          >
            <Redo2 className="h-4 w-4" strokeWidth={ICON_STROKE} />
            {m["collections.redo"]()}
          </button>
        </div>
      </div>

      {error && (
        <div className="mb-4 flex items-center gap-2 rounded-lg border-l-2 border-err bg-err/10 p-3 text-body-sm text-fg">
          <CircleAlert
            className="size-4 shrink-0 text-err"
            aria-hidden="true"
            strokeWidth={ICON_STROKE}
          />
          {error}
        </div>
      )}

      {showFocusToolbar && (
        <FocusToolbar
          slotId={focusedSlot.id}
          availableOperations={textOperations}
          loadError={textToolError}
          onRetry={retryTextTools}
          onSelectOperation={(operation) =>
            setDialogState({ mode: "append", slot: focusedSlot, operation })
          }
        />
      )}

      {loading ? (
        <div className="flex flex-1 items-center justify-center text-body-sm text-fg2">
          {m["collections.loadingCollection"]()}
        </div>
      ) : !collection ? (
        <div className="flex flex-1 items-center justify-center text-body-sm text-fg2">
          {m["collections.notFound"]()}
        </div>
      ) : collection.slots.length === 0 ? (
        <div className="flex flex-1 items-center justify-center text-body-sm text-fg2">
          {m["collections.emptySlots"]()}
        </div>
      ) : (
        <div
          className={
            focusedSlot
              ? "grid grid-cols-1 items-start gap-6 lg:grid-cols-[minmax(0,1fr)_340px]"
              : undefined
          }
        >
          <div className="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3">
            {collection.slots.map((slot) => (
              <SlotItem
                key={slot.id}
                projectId={activeProjectId}
                slot={slot}
                state={slotStates[slot.id]}
                focused={focusedSlot?.id === slot.id}
                onFocus={() => setFocusedSlotId(slot.id)}
                onMediaType={handleMediaType}
              />
            ))}
          </div>

          {focusedSlot && (
            <SlotRecipePanel
              projectId={activeProjectId}
              collectionId={id}
              collectionName={collection.name}
              slotId={focusedSlot.id}
              slotAssetId={focusedSlot.asset_id}
              collectionVersion={collection.version}
              availableOperations={textOperations}
              allOperations={operations}
              operationsLoading={operationsLoading}
              onClose={() => setFocusedSlotId(null)}
              onEditStep={(step, operation, rootAssetId) =>
                setDialogState({
                  mode: "edit",
                  slot: focusedSlot,
                  operation,
                  step,
                  rootAssetId,
                })
              }
            />
          )}
        </div>
      )}

      {dialogState && (
        <TextToolDialog
          open
          projectId={activeProjectId}
          collectionId={id}
          slot={dialogState.slot}
          operation={dialogState.operation}
          mode={dialogState.mode}
          step={dialogState.mode === "edit" ? dialogState.step : undefined}
          rootAssetId={dialogState.mode === "edit" ? dialogState.rootAssetId : undefined}
          onOpenChange={(open) => {
            if (!open) setDialogState(null);
          }}
          onApplied={fetchCollection}
        />
      )}
    </div>
  );
}
