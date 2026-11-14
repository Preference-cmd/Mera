import { useContext, useEffect, useRef, useState } from "react";
import { BookmarkPlus, CircleAlert, Pencil, X } from "lucide-react";
import { useNavigate } from "@tanstack/react-router";
import { QueryClientContext } from "@tanstack/react-query";
import { toast } from "sonner";
import * as m from "$paraglide/messages";
import { ICON_STROKE } from "@/design/tokens";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { libraryQueryKeys } from "@/hooks/useLibrary";
import { opInvoke } from "@/ipc";
import {
  DynamicWorkflowFromRecipeResultSchema,
  RecipeGetResultSchema,
  type ActivityActor,
  type OperationInfo,
  type RecipeGetResult,
  type RecipeStep,
} from "@/ipc/schemas";
import { textOperationLabel } from "@/lib/textOperations";

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function actorLabel(actor: ActivityActor): string {
  switch (actor.kind) {
    case "Human":
      return m["collections.recipe.actor.Human"]();
    case "Agent":
      return m["collections.recipe.actor.Agent"]();
    case "Importer":
      return m["collections.recipe.actor.Importer"]();
    case "System":
      return m["collections.recipe.actor.System"]();
  }
}

function actorBadgeClass(actor: ActivityActor): string {
  if (actor.kind === "Agent") {
    return "border border-agent-line bg-agent-soft text-agent";
  }
  return "bg-tile text-fg2";
}

function InspectSummary({ output }: { output: Record<string, unknown> | null | undefined }) {
  if (!output) return null;
  const title = typeof output.title === "string" && output.title.length > 0 ? output.title : null;
  const stats =
    output.stats && typeof output.stats === "object"
      ? (output.stats as { words?: unknown; lines?: unknown })
      : null;
  const words = typeof stats?.words === "number" ? stats.words : null;
  const lines = typeof stats?.lines === "number" ? stats.lines : null;
  if (!title && words === null && lines === null) return null;

  return (
    <div className="mt-1.5 flex flex-col gap-0.5 text-caption text-fg2">
      {title && <span className="truncate">{m["collections.recipe.inspectTitle"]({ title })}</span>}
      {words !== null && lines !== null && (
        <span className="font-mono">{m["collections.recipe.inspectStats"]({ words, lines })}</span>
      )}
    </div>
  );
}

export function SlotRecipePanel({
  projectId,
  collectionId,
  collectionName,
  slotId,
  slotAssetId,
  collectionVersion,
  availableOperations,
  allOperations,
  onClose,
  onEditStep,
}: {
  projectId: string;
  collectionId: string;
  collectionName?: string;
  slotId: string;
  slotAssetId: string;
  collectionVersion: number;
  availableOperations: OperationInfo[];
  allOperations?: OperationInfo[];
  onClose: () => void;
  onEditStep: (step: RecipeStep, operation: OperationInfo, rootAssetId: string) => void;
}) {
  const navigate = useNavigate();
  const queryClient = useContext(QueryClientContext);
  const [recipe, setRecipe] = useState<RecipeGetResult | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [freezeOpen, setFreezeOpen] = useState(false);
  const [freezeName, setFreezeName] = useState("");
  const [freezing, setFreezing] = useState(false);
  const [freezeError, setFreezeError] = useState<string | null>(null);
  const mountedRef = useRef(true);
  const slotKey = `${projectId}:${collectionId}:${slotId}`;
  const slotKeyRef = useRef(slotKey);
  slotKeyRef.current = slotKey;

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  useEffect(() => {
    setFreezeOpen(false);
    setFreezeError(null);
    setFreezeName("");
    setFreezing(false);
  }, [projectId, collectionId, slotId]);

  useEffect(() => {
    let active = true;
    setLoading(true);
    setError(null);
    opInvoke(projectId, "recipe.get", {
      collection_id: collectionId,
      slot_id: slotId,
    }).then(
      (res) => {
        if (!active) return;
        const parsed = RecipeGetResultSchema.safeParse(res.output);
        if (parsed.success) {
          setRecipe(parsed.data);
        } else {
          setRecipe(null);
          setError(parsed.error.message);
        }
        setLoading(false);
      },
      (err: unknown) => {
        if (!active) return;
        setRecipe(null);
        setError(err instanceof Error ? err.message : String(err));
        setLoading(false);
      },
    );
    return () => {
      active = false;
    };
  }, [projectId, collectionId, slotId, slotAssetId, collectionVersion]);

  return (
    <aside
      aria-label={`${m["collections.recipe.title"]()} ${slotId}`}
      className="flex flex-col rounded-lg border border-hair bg-surface p-4 shadow-1"
    >
      <div className="flex items-center justify-between border-b border-hair pb-3">
        <div className="flex items-center gap-2">
          <span className="font-mono text-caption font-semibold text-accent">
            {m["collections.slotPrefix"]()} {slotId}
          </span>
          <h2 className="text-body-sm font-semibold text-fg">{m["collections.recipe.title"]()}</h2>
        </div>
        <Button
          type="button"
          variant="ghost"
          size="icon-xs"
          aria-label={m["collections.recipe.close"]()}
          title={m["collections.recipe.close"]()}
          onClick={onClose}
        >
          <X aria-hidden="true" strokeWidth={ICON_STROKE} />
        </Button>
      </div>

      {loading ? (
        <div className="py-6 text-caption text-fg2">{m["collections.recipe.loading"]()}</div>
      ) : error ? (
        <div className="mt-3 flex items-start gap-2 rounded-lg border-l-2 border-err bg-err/10 p-3 text-caption text-fg">
          <CircleAlert className="mt-0.5 size-4 shrink-0 text-err" aria-hidden="true" />
          <span>
            {m["collections.recipe.loadError"]()}: {error}
          </span>
        </div>
      ) : !recipe ? (
        <div className="py-6 text-caption text-fg2">{m["collections.recipe.emptySteps"]()}</div>
      ) : (
        <div className="mt-3 flex flex-col gap-4">
          {recipe.stale && (
            <div className="flex items-start gap-2 rounded-lg border-l-2 border-warn bg-warn/10 p-2.5 text-caption text-fg">
              <CircleAlert
                className="mt-0.5 size-4 shrink-0 text-warn"
                aria-hidden="true"
                strokeWidth={ICON_STROKE}
              />
              <span>{m["collections.recipe.staleBanner"]()}</span>
            </div>
          )}

          {/* Root Asset */}
          <div className="rounded-lg border border-hair bg-ground p-3">
            <div className="text-caption font-medium text-fg2">
              {m["collections.recipe.rootAsset"]()}
            </div>
            <div className="mt-1 flex flex-wrap items-center justify-between gap-2 font-mono text-caption text-fg">
              <span className="truncate">{recipe.root_asset.id}</span>
              <span className="text-fg2">
                {recipe.root_asset.media_type} · {formatBytes(recipe.root_asset.size)}
              </span>
            </div>
          </div>

          {/* Steps */}
          <div className="flex flex-col gap-2">
            <div className="text-caption font-medium text-fg2">
              {m["collections.recipe.steps"]()}
            </div>

            {recipe.steps.length === 0 ? (
              <p className="rounded-lg border border-dashed border-hair p-3 text-caption text-fg2">
                {m["collections.recipe.emptySteps"]()}
              </p>
            ) : (
              recipe.steps.map((step, index) => {
                const editableOp = !step.read_only
                  ? availableOperations.find((op) => op.spec.name === step.op)
                  : undefined;

                const header = (
                  <div className="flex flex-wrap items-center justify-between gap-2">
                    <div className="flex items-center gap-1.5">
                      <span className="font-mono text-caption text-fg2">#{index + 1}</span>
                      <span className="text-body-sm font-medium text-fg">
                        {textOperationLabel(step.op, allOperations)}
                      </span>
                    </div>
                    <div className="flex items-center gap-1.5">
                      <span
                        className={`rounded px-1.5 py-0.5 font-mono text-caption ${actorBadgeClass(step.actor)}`}
                      >
                        {actorLabel(step.actor)}
                      </span>
                      {step.stale && (
                        <span className="rounded bg-warn/10 px-1.5 py-0.5 font-mono text-caption text-warn">
                          {m["collections.slotState.stale"]()}
                        </span>
                      )}
                      {editableOp && (
                        <span className="inline-flex items-center gap-1 text-caption text-fg2">
                          <Pencil className="size-3" aria-hidden="true" strokeWidth={ICON_STROKE} />
                          {m["collections.recipe.editStepHint"]()}
                        </span>
                      )}
                    </div>
                  </div>
                );

                if (editableOp) {
                  return (
                    <button
                      key={step.step_id}
                      type="button"
                      data-testid={`recipe-step-${step.step_id}`}
                      onClick={() => onEditStep(step, editableOp, recipe.root_asset.id)}
                      className="flex w-full cursor-pointer flex-col rounded-lg border border-hair bg-ground p-3 text-left transition-colors hover:border-accent/50 hover:bg-hover focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/30"
                    >
                      {header}
                    </button>
                  );
                }

                return (
                  <div
                    key={step.step_id}
                    data-testid={`recipe-step-${step.step_id}`}
                    className="flex flex-col rounded-lg border border-hair bg-ground p-3"
                  >
                    {header}
                    {step.read_only && <InspectSummary output={step.output} />}
                  </div>
                );
              })
            )}
          </div>

          {/* Current Asset */}
          <div className="rounded-lg border border-hair bg-ground p-3">
            <div className="text-caption font-medium text-fg2">
              {m["collections.recipe.currentAsset"]()}
            </div>
            <div className="mt-1 truncate font-mono text-caption text-fg">
              {recipe.current_asset.id}
            </div>
          </div>

          {recipe.steps.length > 0 && !recipe.stale && (
            <div className="flex flex-col gap-2 border-t border-hair pt-3">
              {freezeError && (
                <div className="flex items-start gap-2 rounded-lg border-l-2 border-err bg-err/10 p-2.5 text-caption text-fg">
                  <CircleAlert
                    className="mt-0.5 size-4 shrink-0 text-err"
                    aria-hidden="true"
                    strokeWidth={ICON_STROKE}
                  />
                  <span>
                    {m["collections.recipe.freezeError"]()}: {freezeError}
                  </span>
                </div>
              )}

              {freezeOpen ? (
                <form
                  onSubmit={(event) => {
                    event.preventDefault();
                    const trimmed = freezeName.trim();
                    if (!trimmed || freezing) return;
                    const requestSlotKey = slotKeyRef.current;
                    setFreezing(true);
                    setFreezeError(null);
                    void opInvoke(projectId, "dynamic_workflow.from_recipe", {
                      collection_id: collectionId,
                      slot_id: slotId,
                      name: trimmed,
                    }).then(
                      (res) => {
                        if (!mountedRef.current || slotKeyRef.current !== requestSlotKey) return;
                        const parsed = DynamicWorkflowFromRecipeResultSchema.safeParse(res.output);
                        if (!parsed.success) {
                          setFreezeError(parsed.error.message);
                          setFreezing(false);
                          return;
                        }
                        setFreezing(false);
                        setFreezeOpen(false);
                        if (queryClient) {
                          void queryClient.invalidateQueries({
                            queryKey: libraryQueryKeys.workflows(projectId),
                          });
                        }
                        toast.success(m["collections.recipe.freezeSuccess"](), {
                          action: {
                            label: m["collections.recipe.freezeViewInLibrary"](),
                            onClick: () =>
                              void navigate({ to: "/library", search: { type: "workflow" } }),
                          },
                        });
                      },
                      (err: unknown) => {
                        if (!mountedRef.current || slotKeyRef.current !== requestSlotKey) return;
                        setFreezeError(err instanceof Error ? err.message : String(err));
                        setFreezing(false);
                      },
                    );
                  }}
                  className="flex flex-col gap-2 rounded-lg border border-hair bg-ground p-3"
                >
                  <label htmlFor="recipe-freeze-name" className="text-caption font-medium text-fg2">
                    {m["collections.recipe.freezeNameLabel"]()}
                  </label>
                  <Input
                    id="recipe-freeze-name"
                    value={freezeName}
                    onChange={(event) => setFreezeName(event.target.value)}
                    placeholder={m["collections.recipe.freezeNamePlaceholder"]()}
                    disabled={freezing}
                    className="h-8 bg-surface text-body-sm"
                  />
                  <div className="flex items-center justify-end gap-2 pt-1">
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      disabled={freezing}
                      onClick={() => {
                        setFreezeOpen(false);
                        setFreezeError(null);
                      }}
                    >
                      {m["common.cancel"]()}
                    </Button>
                    <Button
                      type="submit"
                      size="sm"
                      data-testid="recipe-freeze-submit"
                      disabled={freezing || !freezeName.trim()}
                    >
                      {freezing
                        ? m["collections.recipe.freezeSubmitting"]()
                        : m["collections.recipe.freezeSubmit"]()}
                    </Button>
                  </div>
                </form>
              ) : (
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  data-testid="recipe-freeze-open"
                  onClick={() => {
                    setFreezeError(null);
                    setFreezeName(`${collectionName || collectionId} (${slotId})`);
                    setFreezeOpen(true);
                  }}
                  className="w-full justify-center gap-1.5 text-caption"
                >
                  <BookmarkPlus className="size-3.5" strokeWidth={ICON_STROKE} aria-hidden="true" />
                  {m["collections.recipe.freezeOpen"]()}
                </Button>
              )}
            </div>
          )}
        </div>
      )}
    </aside>
  );
}
