import { useEffect, useMemo, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Globe, Shield, ShieldAlert, Terminal, Trash2 } from "lucide-react";
import { toast } from "sonner";
import * as m from "$paraglide/messages";
import { ICON_STROKE } from "@/design/tokens";
import { cn } from "@/lib/utils";
import {
  scriptApproveExecution,
  scriptApproveOnlinePrepare,
  scriptDismissExecution,
  scriptDismissOnlinePrepare,
  scriptRevokeExecution,
  subscribeEvents,
  toCommandError,
} from "@/ipc";
import { queryKeys, useScriptApprovals } from "@/hooks/queries";
import { Button } from "@/components/ui/button";
import { SettingRow, SettingsCard, ValueChip } from "./SettingRow";
import {
  buildScriptReviewQueue,
  ScriptApprovalDialog,
  shortDigest,
} from "@/components/scripts/ScriptApprovalDialog";

function formatError(err: unknown): string {
  const structured = toCommandError(err);
  if (structured) return structured.message;
  if (err instanceof Error && err.message) return err.message;
  return m["scripts.actionFailed"]();
}

function formatApprovedTimestamp(iso: string): string {
  const parsed = new Date(iso);
  if (Number.isNaN(parsed.getTime())) return iso;
  return parsed.toLocaleString();
}

/**
 * Workspace Settings -> Scripts: displays ScriptStep OS sandbox enforcement,
 * any pending script execution / online-prepare requests for this project, and
 * the persisted list of approved SHA-256 execution digests with revocation.
 */
export function WorkspaceScriptsSection({ projectId }: { projectId: string }) {
  const queryClient = useQueryClient();
  const { data: snapshot, isLoading } = useScriptApprovals(projectId);

  const [reviewOpen, setReviewOpen] = useState(false);
  const [selectedKey, setSelectedKey] = useState<string | null>(null);
  const [dialogBusy, setDialogBusy] = useState(false);
  const [revokingDigest, setRevokingDigest] = useState<string | null>(null);

  useEffect(() => {
    if (!projectId) return;
    return subscribeEvents((event) => {
      if (
        event.type === "mod" &&
        (event.kind === "script.approval_requested" ||
          event.kind === "script.online_prepare_requested" ||
          event.kind === "script.approvals_changed")
      ) {
        void queryClient.invalidateQueries({
          queryKey: queryKeys.scriptApprovals(projectId),
        });
      }
    });
  }, [projectId, queryClient]);

  const queue = useMemo(() => buildScriptReviewQueue(snapshot), [snapshot]);
  const approved = snapshot?.approved_executions ?? [];

  const updateSnapshot = (updated: Awaited<ReturnType<typeof scriptApproveExecution>>) => {
    queryClient.setQueryData(queryKeys.scriptApprovals(projectId), updated);
    const remaining = buildScriptReviewQueue(updated);
    if (remaining.length === 0) {
      setReviewOpen(false);
      setSelectedKey(null);
    } else if (!remaining.some((item) => item.key === selectedKey)) {
      setSelectedKey(remaining[0]!.key);
    }
  };

  const handleApproveExecution = async (execDigest: string) => {
    if (dialogBusy) return;
    setDialogBusy(true);
    try {
      const updated = await scriptApproveExecution(projectId, execDigest);
      updateSnapshot(updated);
      toast.success(m["scripts.approvedExecutionToast"]());
    } catch (err) {
      toast.error(formatError(err));
    } finally {
      setDialogBusy(false);
    }
  };

  const handleDismissExecution = async (execDigest: string) => {
    if (dialogBusy) return;
    setDialogBusy(true);
    try {
      const updated = await scriptDismissExecution(projectId, execDigest);
      updateSnapshot(updated);
      toast.info(m["scripts.dismissedToast"]());
    } catch (err) {
      toast.error(formatError(err));
    } finally {
      setDialogBusy(false);
    }
  };

  const handleApproveOnline = async (envHash: string) => {
    if (dialogBusy) return;
    setDialogBusy(true);
    try {
      const updated = await scriptApproveOnlinePrepare(projectId, envHash);
      updateSnapshot(updated);
      toast.success(m["scripts.approvedOnlineToast"]());
    } catch (err) {
      toast.error(formatError(err));
    } finally {
      setDialogBusy(false);
    }
  };

  const handleDismissOnline = async (envHash: string) => {
    if (dialogBusy) return;
    setDialogBusy(true);
    try {
      const updated = await scriptDismissOnlinePrepare(projectId, envHash);
      updateSnapshot(updated);
      toast.info(m["scripts.dismissedToast"]());
    } catch (err) {
      toast.error(formatError(err));
    } finally {
      setDialogBusy(false);
    }
  };

  const handleRevoke = async (execDigest: string) => {
    if (revokingDigest) return;
    setRevokingDigest(execDigest);
    try {
      const updated = await scriptRevokeExecution(projectId, execDigest);
      queryClient.setQueryData(queryKeys.scriptApprovals(projectId), updated);
      toast.success(m["scripts.revokedToast"]());
    } catch (err) {
      toast.error(formatError(err));
    } finally {
      setRevokingDigest(null);
    }
  };

  return (
    <div data-testid="workspace-scripts-section" className="flex flex-col gap-5">
      <p className="text-caption leading-relaxed text-fg2">{m["scripts.settingsDesc"]()}</p>

      {/* Sandbox status */}
      <SettingsCard>
        <SettingRow
          labelId="workspace-scripts-sandbox"
          label={m["scripts.sandboxTitle"]()}
          description={m["scripts.sandboxDesc"]()}
        >
          {snapshot ? (
            <ValueChip data-testid="workspace-scripts-sandbox-chip" className="gap-1.5 font-mono">
              {snapshot.sandbox_enforced ? (
                <Shield className="size-3.5 text-ok" strokeWidth={ICON_STROKE} aria-hidden="true" />
              ) : (
                <ShieldAlert
                  className="size-3.5 text-warn"
                  strokeWidth={ICON_STROKE}
                  aria-hidden="true"
                />
              )}
              <span>
                {snapshot.sandbox_enforced
                  ? m["scripts.sandboxEnforced"]({ backend: snapshot.sandbox_backend })
                  : m["scripts.sandboxUnenforced"]({ backend: snapshot.sandbox_backend })}
              </span>
            </ValueChip>
          ) : (
            <ValueChip>{m["common.loading"]()}</ValueChip>
          )}
        </SettingRow>
      </SettingsCard>

      {/* Pending requests */}
      <section className="flex flex-col gap-2">
        <h3 className="text-body-sm font-medium text-fg">{m["scripts.pendingTitle"]()}</h3>
        <div className="overflow-hidden rounded-xl border border-hair bg-surface">
          {isLoading ? (
            <div className="px-5 py-5 text-center text-caption text-fg3">
              {m["common.loading"]()}
            </div>
          ) : queue.length === 0 ? (
            <div className="px-5 py-5 text-center text-caption text-fg3">
              {m["scripts.pendingEmpty"]()}
            </div>
          ) : (
            <ul>
              {queue.map((item, idx) => {
                const isOnline = item.kind === "online";
                const digest = isOnline ? item.review.env_hash : item.review.exec_digest;
                return (
                  <li
                    key={item.key}
                    data-testid="workspace-script-pending-row"
                    className={cn(
                      "flex items-center justify-between gap-3 px-4 py-3",
                      idx > 0 && "border-t border-hair-soft",
                    )}
                  >
                    <div className="min-w-0 flex-1">
                      <div className="flex flex-wrap items-center gap-2">
                        {isOnline ? (
                          <Globe
                            className="size-3.5 shrink-0 text-warn"
                            strokeWidth={ICON_STROKE}
                            aria-hidden="true"
                          />
                        ) : (
                          <Terminal
                            className="size-3.5 shrink-0 text-warn"
                            strokeWidth={ICON_STROKE}
                            aria-hidden="true"
                          />
                        )}
                        <span className="text-body-sm font-medium text-fg">
                          {isOnline ? m["scripts.kindOnline"]() : m["scripts.kindExecution"]()}
                        </span>
                        <span className="font-mono text-caption text-fg2" title={digest}>
                          {shortDigest(digest)}
                        </span>
                        {item.review.allow_source_build && (
                          <span className="rounded bg-warn/15 px-1.5 py-0.5 text-[11px] font-medium text-warn">
                            {m["scripts.sourceBuildBadge"]()}
                          </span>
                        )}
                      </div>
                      <div className="mt-0.5 truncate text-caption text-fg3">
                        Python {item.review.python || "default"}
                        {item.review.dependencies.length > 0
                          ? ` · ${item.review.dependencies.join(", ")}`
                          : ` · ${m["scripts.dependenciesNone"]()}`}
                      </div>
                    </div>
                    <Button
                      type="button"
                      variant="outline"
                      size="sm"
                      className="h-7 shrink-0 px-2.5 text-caption"
                      onClick={() => {
                        setSelectedKey(item.key);
                        setReviewOpen(true);
                      }}
                    >
                      {m["scripts.review"]()}
                    </Button>
                  </li>
                );
              })}
            </ul>
          )}
        </div>
      </section>

      {/* Persisted approved script digests */}
      <section className="flex flex-col gap-2">
        <h3 className="text-body-sm font-medium text-fg">{m["scripts.approvedTitle"]()}</h3>
        <div className="overflow-hidden rounded-xl border border-hair bg-surface">
          {isLoading ? (
            <div className="px-5 py-5 text-center text-caption text-fg3">
              {m["common.loading"]()}
            </div>
          ) : approved.length === 0 ? (
            <div className="px-5 py-5 text-center text-caption text-fg3">
              {m["scripts.approvedEmpty"]()}
            </div>
          ) : (
            <ul>
              {approved.map((record, idx) => {
                const isRevoking = revokingDigest === record.exec_digest;
                return (
                  <li
                    key={record.exec_digest}
                    data-testid="workspace-script-approved-row"
                    className={cn(
                      "flex items-center justify-between gap-3 px-4 py-3",
                      idx > 0 && "border-t border-hair-soft",
                    )}
                  >
                    <div className="min-w-0 flex-1">
                      <div className="flex flex-wrap items-center gap-2">
                        <span
                          className="font-mono text-caption font-medium text-fg"
                          title={record.exec_digest}
                        >
                          {shortDigest(record.exec_digest)}
                        </span>
                        {record.python && (
                          <span className="rounded bg-tile px-1.5 py-0.5 font-mono text-[11px] text-fg2">
                            Python {record.python}
                          </span>
                        )}
                        {record.allow_source_build && (
                          <span className="rounded bg-warn/15 px-1.5 py-0.5 text-[11px] font-medium text-warn">
                            {m["scripts.sourceBuildBadge"]()}
                          </span>
                        )}
                      </div>
                      <div className="mt-1 flex flex-wrap items-center gap-x-2 gap-y-0.5 text-caption text-fg3">
                        <span>
                          {record.dependencies.length > 0
                            ? record.dependencies.join(", ")
                            : m["scripts.dependenciesNone"]()}
                        </span>
                        <span>·</span>
                        <span>
                          {m["scripts.approvedAt"]({
                            time: formatApprovedTimestamp(record.approved_at),
                          })}
                        </span>
                      </div>
                    </div>
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      disabled={Boolean(revokingDigest)}
                      data-testid={`revoke-script-${record.exec_digest}`}
                      onClick={() => void handleRevoke(record.exec_digest)}
                      className="h-7 shrink-0 gap-1 px-2 text-caption text-fg2 hover:text-err"
                    >
                      <Trash2 className="size-3.5" strokeWidth={ICON_STROKE} aria-hidden="true" />
                      {isRevoking ? m["scripts.revoking"]() : m["scripts.revoke"]()}
                    </Button>
                  </li>
                );
              })}
            </ul>
          )}
        </div>
      </section>

      <ScriptApprovalDialog
        open={reviewOpen}
        onOpenChange={setReviewOpen}
        snapshot={snapshot}
        selectedKey={selectedKey}
        onSelectKey={setSelectedKey}
        busy={dialogBusy}
        onApproveExecution={handleApproveExecution}
        onDismissExecution={handleDismissExecution}
        onApproveOnline={handleApproveOnline}
        onDismissOnline={handleDismissOnline}
      />
    </div>
  );
}
