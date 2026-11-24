import { useEffect, useMemo, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { ShieldAlert } from "lucide-react";
import { toast } from "sonner";
import * as m from "$paraglide/messages";
import { ICON_STROKE } from "@/design/tokens";
import {
  scriptApproveExecution,
  scriptApproveOnlinePrepare,
  scriptDismissExecution,
  scriptDismissOnlinePrepare,
  subscribeEvents,
  toCommandError,
} from "@/ipc";
import { queryKeys, useScriptApprovals } from "@/hooks/queries";
import { useProjectStore } from "@/store/project";
import { isShellModalOpen, useUIStore } from "@/store/uiStore";
import { Button } from "@/components/ui/button";
import {
  buildScriptReviewQueue,
  ScriptApprovalDialog,
} from "@/components/scripts/ScriptApprovalDialog";

function formatError(err: unknown): string {
  const structured = toCommandError(err);
  if (structured) return structured.message;
  if (err instanceof Error && err.message) return err.message;
  return m["scripts.actionFailed"]();
}

function eventProjectId(payload: unknown): string | null {
  if (!payload || typeof payload !== "object") return null;
  const value = (payload as { projectId?: unknown }).projectId;
  return typeof value === "string" ? value : null;
}

/**
 * Slim script-approval banner and live review modal mounted directly below
 * StorageBanners in AppShell. Listens for `script.approval_requested`,
 * `script.online_prepare_requested`, and `script.approvals_changed` events so
 * human-in-the-loop prompts surface immediately whether triggered by the UI or
 * by an external MCP agent.
 */
export function ScriptApprovalBanner() {
  const queryClient = useQueryClient();
  const activeProjectId = useProjectStore((s) => s.activeProjectId);
  const openSettings = useUIStore((s) => s.openSettings);
  const { data: snapshot } = useScriptApprovals(activeProjectId);

  const [dialogOpen, setDialogOpen] = useState(false);
  const [selectedKey, setSelectedKey] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // Track requests that already auto-opened the dialog so dismissing the modal
  // keeps only the non-intrusive banner without re-popping repeatedly.
  const autoOpenedKeysRef = useRef<Set<string>>(new Set());
  const prevQueueKeysRef = useRef<Set<string>>(new Set());

  const queue = useMemo(() => buildScriptReviewQueue(snapshot), [snapshot]);

  useEffect(() => {
    autoOpenedKeysRef.current.clear();
    prevQueueKeysRef.current.clear();
    setDialogOpen(false);
    setSelectedKey(null);
  }, [activeProjectId]);

  useEffect(() => {
    if (!activeProjectId) return;
    return subscribeEvents((event) => {
      if (event.type !== "mod") return;
      if (
        event.kind !== "script.approval_requested" &&
        event.kind !== "script.online_prepare_requested" &&
        event.kind !== "script.approvals_changed"
      ) {
        return;
      }
      const targetProject = eventProjectId(event.payload);
      if (targetProject && targetProject !== activeProjectId) return;

      void queryClient.invalidateQueries({
        queryKey: queryKeys.scriptApprovals(activeProjectId),
      });

      if (
        (event.kind === "script.approval_requested" ||
          event.kind === "script.online_prepare_requested") &&
        !isShellModalOpen()
      ) {
        const review = (event.payload as { review?: Record<string, unknown> } | null)?.review;
        const key =
          event.kind === "script.online_prepare_requested" && typeof review?.env_hash === "string"
            ? `online:${review.env_hash}`
            : typeof review?.exec_digest === "string"
              ? `exec:${review.exec_digest}`
              : null;
        if (key && !autoOpenedKeysRef.current.has(key)) {
          autoOpenedKeysRef.current.add(key);
          setSelectedKey(key);
          setDialogOpen(true);
        }
      }
    });
  }, [activeProjectId, queryClient]);

  // Prune auto-open tracking for items that left the pending queue (approved,
  // dismissed, or resolved elsewhere) so a later revoke/re-request auto-opens
  // again. Also close the modal cleanly when a non-empty queue drains to empty,
  // without cancelling a freshly auto-opened dialog while its query refresh is
  // still in flight.
  useEffect(() => {
    const currentKeys = new Set(queue.map((item) => item.key));
    for (const prevKey of prevQueueKeysRef.current) {
      if (!currentKeys.has(prevKey)) {
        autoOpenedKeysRef.current.delete(prevKey);
      }
    }
    if (dialogOpen && prevQueueKeysRef.current.size > 0 && currentKeys.size === 0) {
      setDialogOpen(false);
      setSelectedKey(null);
    }
    prevQueueKeysRef.current = currentKeys;
  }, [dialogOpen, queue]);

  const applyUpdatedSnapshot = (
    updated: Awaited<ReturnType<typeof scriptApproveExecution>>,
    handledKey: string,
  ) => {
    autoOpenedKeysRef.current.delete(handledKey);
    queryClient.setQueryData(queryKeys.scriptApprovals(activeProjectId), updated);
    const remaining = buildScriptReviewQueue(updated);
    if (remaining.length === 0) {
      setDialogOpen(false);
      setSelectedKey(null);
    } else if (!remaining.some((item) => item.key === selectedKey)) {
      setSelectedKey(remaining[0]!.key);
    }
  };

  const handleApproveExecution = async (execDigest: string) => {
    if (busy) return;
    setBusy(true);
    try {
      const updated = await scriptApproveExecution(activeProjectId, execDigest);
      applyUpdatedSnapshot(updated, `exec:${execDigest}`);
      toast.success(m["scripts.approvedExecutionToast"]());
    } catch (err) {
      toast.error(formatError(err));
    } finally {
      setBusy(false);
    }
  };

  const handleDismissExecution = async (execDigest: string) => {
    if (busy) return;
    setBusy(true);
    try {
      const updated = await scriptDismissExecution(activeProjectId, execDigest);
      applyUpdatedSnapshot(updated, `exec:${execDigest}`);
      toast.info(m["scripts.dismissedToast"]());
    } catch (err) {
      toast.error(formatError(err));
    } finally {
      setBusy(false);
    }
  };

  const handleApproveOnline = async (envHash: string) => {
    if (busy) return;
    setBusy(true);
    try {
      const updated = await scriptApproveOnlinePrepare(activeProjectId, envHash);
      applyUpdatedSnapshot(updated, `online:${envHash}`);
      toast.success(m["scripts.approvedOnlineToast"]());
    } catch (err) {
      toast.error(formatError(err));
    } finally {
      setBusy(false);
    }
  };

  const handleDismissOnline = async (envHash: string) => {
    if (busy) return;
    setBusy(true);
    try {
      const updated = await scriptDismissOnlinePrepare(activeProjectId, envHash);
      applyUpdatedSnapshot(updated, `online:${envHash}`);
      toast.info(m["scripts.dismissedToast"]());
    } catch (err) {
      toast.error(formatError(err));
    } finally {
      setBusy(false);
    }
  };

  if (queue.length === 0) {
    return null;
  }

  return (
    <>
      <div
        role="alert"
        data-testid="script-approval-banner"
        className="relative z-10 flex shrink-0 items-center justify-between gap-3 border-b border-hair-soft bg-warn/10 px-4 py-1.5 text-caption text-fg"
      >
        <div className="flex min-w-0 items-center gap-2">
          <ShieldAlert className="size-3.5 shrink-0 text-warn" strokeWidth={ICON_STROKE} />
          <span className="truncate font-medium">
            {queue.length === 1
              ? m["scripts.bannerSingle"]()
              : m["scripts.bannerMultiple"]({ count: queue.length })}
          </span>
        </div>
        <div className="flex shrink-0 items-center gap-1.5">
          <Button
            type="button"
            size="sm"
            variant="ghost"
            onClick={() => openSettings("workspace-scripts", activeProjectId)}
            className="h-6 px-2 text-caption text-fg2 hover:text-fg"
          >
            {m["scripts.bannerOpenSettings"]()}
          </Button>
          <Button
            type="button"
            size="sm"
            variant="outline"
            onClick={() => {
              setSelectedKey(queue[0]!.key);
              setDialogOpen(true);
            }}
            className="h-6 border-warn/40 bg-surface px-2.5 text-caption text-fg hover:bg-hover"
            data-testid="script-approval-banner-review"
          >
            {m["scripts.bannerReview"]()}
          </Button>
        </div>
      </div>

      <ScriptApprovalDialog
        open={dialogOpen}
        onOpenChange={setDialogOpen}
        snapshot={snapshot}
        selectedKey={selectedKey}
        onSelectKey={setSelectedKey}
        busy={busy}
        onApproveExecution={handleApproveExecution}
        onDismissExecution={handleDismissExecution}
        onApproveOnline={handleApproveOnline}
        onDismissOnline={handleDismissOnline}
      />
    </>
  );
}
