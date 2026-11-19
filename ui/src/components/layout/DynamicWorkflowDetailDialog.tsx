import { useEffect, useState } from "react";
import { CircleAlert, Loader2 } from "lucide-react";
import { useNavigate } from "@tanstack/react-router";
import * as m from "$paraglide/messages";
import { ICON_STROKE } from "@/design/tokens";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { extractModNamespace, isOperationAvailable, useOperations } from "@/hooks/useOperations";
import { opInvoke } from "@/ipc";
import { DynamicWorkflowGetResultSchema, type DynamicWorkflowGetResult } from "@/ipc/schemas";
import { textOperationLabel } from "@/lib/textOperations";

function formatBindingValue(value: unknown): string {
  if (value && typeof value === "object" && !Array.isArray(value) && "$from" in value) {
    return `← ${String((value as { $from: unknown }).$from)}`;
  }
  if (typeof value === "string") return JSON.stringify(value);
  if (typeof value === "number" || typeof value === "boolean" || value === null) {
    return String(value);
  }
  try {
    const serialized = JSON.stringify(value);
    return serialized.length > 72 ? `${serialized.slice(0, 69)}…` : serialized;
  } catch {
    return String(value);
  }
}

export function DynamicWorkflowDetailDialog({
  open,
  onOpenChange,
  projectId,
  workflowId,
  fallbackName,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  projectId: string;
  workflowId: string;
  fallbackName: string;
}) {
  const navigate = useNavigate();
  const { operations, loading: operationsLoading } = useOperations(projectId);
  const [workflow, setWorkflow] = useState<DynamicWorkflowGetResult | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open || !workflowId) return;
    let active = true;
    setLoading(true);
    setError(null);
    setWorkflow(null);

    opInvoke(projectId, "dynamic_workflow.get", { dynamic_workflow_id: workflowId }).then(
      (res) => {
        if (!active) return;
        const parsed = DynamicWorkflowGetResultSchema.safeParse(res.output);
        if (parsed.success) {
          setWorkflow(parsed.data);
        } else {
          setError(parsed.error.message);
        }
        setLoading(false);
      },
      (err: unknown) => {
        if (!active) return;
        setError(err instanceof Error ? err.message : String(err));
        setLoading(false);
      },
    );

    return () => {
      active = false;
    };
  }, [open, projectId, workflowId]);

  const inputKeys = workflow ? Object.keys(workflow.workflow.inputs) : [];
  const paramsProps =
    workflow?.workflow.params &&
    typeof workflow.workflow.params === "object" &&
    "properties" in workflow.workflow.params &&
    workflow.workflow.params.properties &&
    typeof workflow.workflow.params.properties === "object" &&
    !Array.isArray(workflow.workflow.params.properties)
      ? Object.keys(workflow.workflow.params.properties as Record<string, unknown>)
      : [];
  const outputEntries = workflow ? Object.entries(workflow.workflow.output) : [];

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        className="max-h-[85vh] overflow-y-auto sm:max-w-xl"
        data-testid="dynamic-workflow-detail-dialog"
        {...(workflow?.description ? {} : { "aria-describedby": undefined })}
      >
        <DialogHeader>
          <DialogTitle>{workflow?.name ?? fallbackName}</DialogTitle>
          {workflow?.description && <DialogDescription>{workflow.description}</DialogDescription>}
          {workflow && (
            <div className="pt-1">
              <span className="inline-flex items-center rounded bg-tile px-2 py-0.5 font-mono text-caption text-fg2">
                {`dynamic_workflow.${workflow.id} · v${workflow.version}`}
              </span>
            </div>
          )}
        </DialogHeader>

        {loading ? (
          <div className="flex items-center gap-2 py-6 text-body-sm text-fg2">
            <Loader2 className="size-4 animate-spin" strokeWidth={ICON_STROKE} aria-hidden="true" />
            <span>{m["library.workflowDialog.loading"]()}</span>
          </div>
        ) : error ? (
          <div className="flex items-start gap-2 rounded-lg border-l-2 border-err bg-err/10 p-3 text-caption text-fg">
            <CircleAlert
              className="mt-0.5 size-4 shrink-0 text-err"
              aria-hidden="true"
              strokeWidth={ICON_STROKE}
            />
            <span>
              {m["library.workflowDialog.loadError"]()}: {error}
            </span>
          </div>
        ) : workflow ? (
          <div className="flex flex-col gap-4">
            {/* Inputs & Parameters */}
            <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
              <div className="rounded-lg border border-hair bg-ground p-3">
                <div className="text-caption font-medium text-fg2">
                  {m["library.workflowDialog.inputs"]()}
                </div>
                {inputKeys.length === 0 ? (
                  <p className="mt-1 text-caption text-fg3">
                    {m["library.workflowDialog.noInputs"]()}
                  </p>
                ) : (
                  <div className="mt-1.5 flex flex-wrap gap-1.5">
                    {inputKeys.map((key) => (
                      <span
                        key={key}
                        className="rounded bg-tile px-1.5 py-0.5 font-mono text-caption text-fg"
                      >
                        {key}
                      </span>
                    ))}
                  </div>
                )}
              </div>

              <div className="rounded-lg border border-hair bg-ground p-3">
                <div className="text-caption font-medium text-fg2">
                  {m["library.workflowDialog.params"]()}
                </div>
                {paramsProps.length === 0 ? (
                  <p className="mt-1 text-caption text-fg3">
                    {m["library.workflowDialog.noInputs"]()}
                  </p>
                ) : (
                  <div className="mt-1.5 flex flex-wrap gap-1.5">
                    {paramsProps.map((key) => (
                      <span
                        key={key}
                        className="rounded bg-tile px-1.5 py-0.5 font-mono text-caption text-fg"
                      >
                        {key}
                      </span>
                    ))}
                  </div>
                )}
              </div>
            </div>

            {/* Steps */}
            <div className="flex flex-col gap-2">
              <div className="text-caption font-medium text-fg2">
                {m["library.workflowDialog.steps"]()}
              </div>
              {workflow.workflow.steps.length === 0 ? (
                <p className="rounded-lg border border-dashed border-hair p-3 text-caption text-fg2">
                  {m["library.workflowDialog.emptySteps"]()}
                </p>
              ) : (
                <ol className="flex flex-col gap-2">
                  {workflow.workflow.steps.map((step, index) => {
                    const inputEntries = Object.entries(step.input);
                    const modNamespace = extractModNamespace(step.op);
                    const matchingOp = operations.find((op) => op.spec.name === step.op);
                    const missingModId =
                      !operationsLoading &&
                      modNamespace !== null &&
                      !isOperationAvailable(matchingOp)
                        ? modNamespace
                        : null;
                    return (
                      <li
                        key={step.id}
                        data-testid={`workflow-detail-step-${step.id}`}
                        className="flex flex-col gap-1.5 rounded-lg border border-hair bg-ground p-3"
                      >
                        <div className="flex flex-wrap items-center justify-between gap-2">
                          <div className="flex items-center gap-1.5">
                            <span className="font-mono text-caption text-fg2">#{index + 1}</span>
                            <span className="text-body-sm font-medium text-fg">
                              {textOperationLabel(step.op, operations)}
                            </span>
                          </div>
                          <span className="rounded bg-tile px-1.5 py-0.5 font-mono text-caption text-fg2">
                            {step.id} · {step.op}
                          </span>
                        </div>

                        {missingModId && (
                          <div
                            data-testid={`workflow-step-missing-mod-${step.id}`}
                            className="flex flex-wrap items-center justify-between gap-2 rounded-md border border-warn/40 bg-warn/10 px-2.5 py-1.5 text-caption text-fg"
                          >
                            <span className="inline-flex items-center gap-1.5">
                              <CircleAlert
                                className="size-3.5 shrink-0 text-warn"
                                aria-hidden="true"
                                strokeWidth={ICON_STROKE}
                              />
                              {m["collections.recipe.missingMod"]({ modId: missingModId })}
                            </span>
                            <Button
                              type="button"
                              variant="outline"
                              size="sm"
                              className="h-6 px-2 text-caption"
                              onClick={() => {
                                onOpenChange(false);
                                void navigate({ to: "/mods" });
                              }}
                            >
                              {m["collections.recipe.manageMods"]()}
                            </Button>
                          </div>
                        )}

                        {inputEntries.length > 0 && (
                          <div className="flex flex-col gap-1 border-t border-hair-soft pt-1.5 font-mono text-caption text-fg2">
                            {inputEntries.map(([key, value]) => (
                              <div key={key} className="flex items-baseline gap-2 truncate">
                                <span className="shrink-0 text-fg3">{key}:</span>
                                <span className="truncate text-fg">
                                  {formatBindingValue(value)}
                                </span>
                              </div>
                            ))}
                          </div>
                        )}
                      </li>
                    );
                  })}
                </ol>
              )}
            </div>

            {/* Output */}
            {outputEntries.length > 0 && (
              <div className="rounded-lg border border-hair bg-ground p-3">
                <div className="text-caption font-medium text-fg2">
                  {m["library.workflowDialog.output"]()}
                </div>
                <div className="mt-1.5 flex flex-col gap-1 font-mono text-caption text-fg">
                  {outputEntries.map(([key, value]) => (
                    <div key={key} className="flex items-baseline gap-2 truncate">
                      <span className="shrink-0 text-fg2">{key}:</span>
                      <span className="truncate">{formatBindingValue(value)}</span>
                    </div>
                  ))}
                </div>
              </div>
            )}
          </div>
        ) : null}
      </DialogContent>
    </Dialog>
  );
}
