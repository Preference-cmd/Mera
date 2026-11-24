import { useMemo, useState } from "react";
import {
  Check,
  ChevronLeft,
  ChevronRight,
  Copy,
  Globe,
  Shield,
  ShieldAlert,
  Terminal,
} from "lucide-react";
import * as m from "$paraglide/messages";
import { ICON_STROKE } from "@/design/tokens";
import type {
  OnlinePrepareReview,
  ScriptApprovalReview,
  ScriptApprovalsSnapshotDto,
} from "@/ipc/schemas";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";

export type ScriptReviewQueueItem =
  | { kind: "online"; key: string; review: OnlinePrepareReview }
  | { kind: "execution"; key: string; review: ScriptApprovalReview };

export function buildScriptReviewQueue(
  snapshot: ScriptApprovalsSnapshotDto | null | undefined,
): ScriptReviewQueueItem[] {
  if (!snapshot) return [];
  const items: ScriptReviewQueueItem[] = [];
  for (const review of snapshot.pending_online_prepares) {
    items.push({ kind: "online", key: `online:${review.env_hash}`, review });
  }
  for (const review of snapshot.pending_executions) {
    items.push({ kind: "execution", key: `exec:${review.exec_digest}`, review });
  }
  return items;
}

export function shortDigest(hex: string): string {
  return hex.length > 16 ? `${hex.slice(0, 12)}…${hex.slice(-4)}` : hex;
}

function CodeBlockWithLines({ code }: { code: string }) {
  const [copied, setCopied] = useState(false);
  const lines = useMemo(() => {
    const split = code.replace(/\r\n/g, "\n").split("\n");
    return split.length > 0 ? split : [""];
  }, [code]);

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(code);
      setCopied(true);
      setTimeout(() => setCopied(false), 1600);
    } catch {
      // Clipboard API may be unavailable in headless tests; ignore silently.
    }
  };

  return (
    <div className="overflow-hidden rounded-lg border border-hair bg-ground">
      <div className="flex items-center justify-between border-b border-hair-soft px-3 py-1.5">
        <span className="text-caption font-medium text-fg2">{m["scripts.sourceLabel"]()}</span>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          onClick={() => void handleCopy()}
          className="h-6 gap-1 px-2 text-caption text-fg2"
        >
          {copied ? (
            <Check className="size-3.5 text-ok" strokeWidth={ICON_STROKE} aria-hidden="true" />
          ) : (
            <Copy className="size-3.5" strokeWidth={ICON_STROKE} aria-hidden="true" />
          )}
          {copied ? m["scripts.copiedCode"]() : m["scripts.copyCode"]()}
        </Button>
      </div>
      <div
        data-testid="script-approval-code"
        className="max-h-72 overflow-auto bg-app/50 p-3 font-mono text-[12.5px] leading-5 text-fg"
      >
        <table className="w-full border-collapse">
          <tbody>
            {lines.map((line, index) => (
              <tr key={index}>
                <td
                  aria-hidden="true"
                  className="w-9 select-none pr-3 text-right align-top text-fg4"
                >
                  {index + 1}
                </td>
                <td className="whitespace-pre align-top">{line.length > 0 ? line : " "}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

function DependenciesChips({ dependencies }: { dependencies: string[] }) {
  if (dependencies.length === 0) {
    return <span className="text-caption text-fg3">{m["scripts.dependenciesNone"]()}</span>;
  }
  return (
    <div className="flex flex-wrap gap-1.5">
      {dependencies.map((dep) => (
        <span
          key={dep}
          className="rounded border border-hair-soft bg-tile px-1.5 py-0.5 font-mono text-caption text-fg"
        >
          {dep}
        </span>
      ))}
    </div>
  );
}

/**
 * Confirmation modal for `script.python` two-phase human-in-the-loop reviews:
 *   1) Online dependency preparation (host-side network access via `uv`).
 *   2) First-run script execution approval bound to the exact SHA-256 digest.
 */
export function ScriptApprovalDialog({
  open,
  onOpenChange,
  snapshot,
  selectedKey,
  onSelectKey,
  busy,
  onApproveExecution,
  onDismissExecution,
  onApproveOnline,
  onDismissOnline,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  snapshot: ScriptApprovalsSnapshotDto | null | undefined;
  selectedKey?: string | null;
  onSelectKey?: (key: string) => void;
  busy: boolean;
  onApproveExecution: (execDigest: string) => Promise<void> | void;
  onDismissExecution: (execDigest: string) => Promise<void> | void;
  onApproveOnline: (envHash: string) => Promise<void> | void;
  onDismissOnline: (envHash: string) => Promise<void> | void;
}) {
  const queue = useMemo(() => buildScriptReviewQueue(snapshot), [snapshot]);
  const [internalIndex, setInternalIndex] = useState(0);

  if (!open || !snapshot || queue.length === 0) return null;

  const resolvedIndex = (() => {
    if (selectedKey) {
      const idx = queue.findIndex((item) => item.key === selectedKey);
      if (idx >= 0) return idx;
    }
    return Math.min(internalIndex, queue.length - 1);
  })();

  const active = queue[resolvedIndex]!;

  const goTo = (nextIndex: number) => {
    const clamped = Math.max(0, Math.min(queue.length - 1, nextIndex));
    setInternalIndex(clamped);
    onSelectKey?.(queue[clamped]!.key);
  };

  const isOnline = active.kind === "online";

  return (
    <Dialog open={open} onOpenChange={(next) => !busy && onOpenChange(next)}>
      <DialogContent
        showCloseButton={!busy}
        data-testid="script-approval-dialog"
        className="sm:max-w-2xl"
      >
        <DialogHeader>
          <div className="flex flex-wrap items-center justify-between gap-2 pr-6">
            <div className="inline-flex items-center gap-1.5 rounded border border-hair bg-tile px-2 py-0.5 text-caption font-medium text-fg2">
              {isOnline ? (
                <Globe className="size-3.5 text-fg2" strokeWidth={ICON_STROKE} aria-hidden="true" />
              ) : (
                <Terminal
                  className="size-3.5 text-fg2"
                  strokeWidth={ICON_STROKE}
                  aria-hidden="true"
                />
              )}
              <span>{isOnline ? m["scripts.kindOnline"]() : m["scripts.kindExecution"]()}</span>
            </div>

            {queue.length > 1 && (
              <div
                data-testid="script-approval-queue-nav"
                className="flex items-center gap-1 text-caption text-fg2"
              >
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-xs"
                  disabled={busy || resolvedIndex === 0}
                  aria-label={m["scripts.queuePrev"]()}
                  onClick={() => goTo(resolvedIndex - 1)}
                >
                  <ChevronLeft className="size-3.5" strokeWidth={ICON_STROKE} />
                </Button>
                <span className="tabular-nums">
                  {m["scripts.queueCounter"]({
                    current: resolvedIndex + 1,
                    total: queue.length,
                  })}
                </span>
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-xs"
                  disabled={busy || resolvedIndex >= queue.length - 1}
                  aria-label={m["scripts.queueNext"]()}
                  onClick={() => goTo(resolvedIndex + 1)}
                >
                  <ChevronRight className="size-3.5" strokeWidth={ICON_STROKE} />
                </Button>
              </div>
            )}
          </div>

          <DialogTitle>
            {isOnline ? m["scripts.dialogTitleOnline"]() : m["scripts.dialogTitleExecution"]()}
          </DialogTitle>
          <DialogDescription>
            {isOnline ? m["scripts.dialogDescOnline"]() : m["scripts.dialogDescExecution"]()}
          </DialogDescription>
        </DialogHeader>

        <div className="flex max-h-[62vh] flex-col gap-3 overflow-y-auto pr-1">
          {/* Sandbox status bar */}
          <div className="flex items-center gap-2 rounded-lg border border-hair bg-ground px-3 py-2 text-caption text-fg">
            {snapshot.sandbox_enforced ? (
              <Shield
                className="size-4 shrink-0 text-ok"
                strokeWidth={ICON_STROKE}
                aria-hidden="true"
              />
            ) : (
              <ShieldAlert
                className="size-4 shrink-0 text-warn"
                strokeWidth={ICON_STROKE}
                aria-hidden="true"
              />
            )}
            <span>
              {snapshot.sandbox_enforced
                ? m["scripts.sandboxEnforced"]({ backend: snapshot.sandbox_backend })
                : m["scripts.sandboxUnenforced"]({ backend: snapshot.sandbox_backend })}
            </span>
          </div>

          {/* Source-build warning when explicitly opted in */}
          {active.review.allow_source_build && (
            <div
              role="alert"
              data-testid="script-approval-source-build-warning"
              className="flex items-start gap-2 rounded-lg border border-warn/40 bg-warn/10 p-3 text-caption text-fg"
            >
              <ShieldAlert
                className="mt-0.5 size-4 shrink-0 text-warn"
                strokeWidth={ICON_STROKE}
                aria-hidden="true"
              />
              <span>{m["scripts.sourceBuildWarning"]()}</span>
            </div>
          )}

          {/* Metadata card */}
          <div className="space-y-2 rounded-lg border border-hair bg-ground p-3 text-caption">
            <div className="flex flex-wrap items-baseline justify-between gap-2">
              <span className="font-medium text-fg">{m["scripts.pythonLabel"]()}</span>
              <span className="font-mono text-fg2">
                {active.review.python || "default"}
                {isOnline && active.review.no_build ? ` · ${m["scripts.noBuildWheelOnly"]()}` : ""}
              </span>
            </div>

            <div className="flex flex-col gap-1 border-t border-hair-soft pt-2">
              <span className="font-medium text-fg">{m["scripts.dependenciesLabel"]()}</span>
              <DependenciesChips dependencies={active.review.dependencies} />
            </div>

            <div className="flex flex-col gap-0.5 border-t border-hair-soft pt-2">
              <span className="font-medium text-fg">
                {isOnline ? m["scripts.envHashLabel"]() : m["scripts.digestLabel"]()}
              </span>
              <span data-testid="script-approval-digest" className="break-all font-mono text-fg2">
                {isOnline ? active.review.env_hash : active.review.exec_digest}
              </span>
            </div>

            {!isOnline && active.review.lock_sha256 && (
              <div className="flex flex-col gap-0.5 border-t border-hair-soft pt-2">
                <span className="font-medium text-fg">{m["scripts.lockHashLabel"]()}</span>
                <span className="break-all font-mono text-fg2">{active.review.lock_sha256}</span>
              </div>
            )}

            {isOnline && (active.review.missing_uv || active.review.missing_managed_python) && (
              <ul className="list-disc space-y-1 border-t border-hair-soft pt-2 pl-4 text-fg2">
                {active.review.missing_uv && <li>{m["scripts.missingUvNotice"]()}</li>}
                {active.review.missing_managed_python && (
                  <li>{m["scripts.missingPythonNotice"]({ version: active.review.python })}</li>
                )}
              </ul>
            )}
          </div>

          {/* Full code view for execution approvals */}
          {!isOnline && <CodeBlockWithLines code={active.review.code} />}

          {/* Persistence / scope explanation */}
          <p className="text-caption leading-relaxed text-fg3">
            {isOnline ? m["scripts.onlineOnceNote"]() : m["scripts.persistNote"]()}
          </p>
        </div>

        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            disabled={busy}
            data-testid="script-approval-dismiss"
            onClick={() => {
              if (isOnline) {
                void onDismissOnline(active.review.env_hash);
              } else {
                void onDismissExecution(active.review.exec_digest);
              }
            }}
          >
            {m["scripts.dismiss"]()}
          </Button>
          <Button
            type="button"
            disabled={busy}
            data-testid="script-approval-confirm"
            onClick={() => {
              if (isOnline) {
                void onApproveOnline(active.review.env_hash);
              } else {
                void onApproveExecution(active.review.exec_digest);
              }
            }}
          >
            {busy
              ? m["scripts.approving"]()
              : isOnline
                ? m["scripts.approveOnline"]()
                : m["scripts.approveExecution"]()}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
