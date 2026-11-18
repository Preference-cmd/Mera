import { useEffect, useRef, useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { useQueryClient } from "@tanstack/react-query";
import { CircleAlert, Loader2, Trash2 } from "lucide-react";
import { toast } from "sonner";
import * as m from "$paraglide/messages";
import { ICON_STROKE } from "@/design/tokens";
import { cn } from "@/lib/utils";
import { formatBytes } from "@/lib/library";
import { opInvoke, storageCleanCache, storageSetBudget } from "@/ipc";
import type { StorageProjectUsageDto } from "@/ipc/schemas";
import { queryKeys, useStorageStatus } from "@/hooks/queries";
import { GLOBAL_LIBRARY_QUERY_KEY } from "@/hooks/useGlobalLibrary";
import { useProjectStore } from "@/store/project";
import { useUIStore } from "@/store/uiStore";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { SettingRow, SettingsCard, ValueChip } from "./SettingRow";

const GIB = 1024 * 1024 * 1024;

function formatLimitGbDraft(limitBytes: number | null): string {
  if (limitBytes === null) return "";
  const gb = limitBytes / GIB;
  return Number.isInteger(gb) ? String(gb) : String(Math.round(gb * 100) / 100);
}

/** Settings -> App -> Storage management section. */
export function StorageSettingsSection() {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const activeProjectId = useProjectStore((s) => s.activeProjectId);
  const deleteProject = useProjectStore((s) => s.deleteProject);
  const closeSettings = useUIStore((s) => s.closeSettings);
  const setSettingsSection = useUIStore((s) => s.setSettingsSection);

  const { data: status, isLoading, error, refetch } = useStorageStatus();

  const [limitDraft, setLimitDraft] = useState("");
  const [limitError, setLimitError] = useState<string | null>(null);
  const [savingBudget, setSavingBudget] = useState(false);
  const budgetInFlight = useRef(false);

  const [cleaningCache, setCleaningCache] = useState(false);
  const [purgeDialogOpen, setPurgeDialogOpen] = useState(false);
  const [purgingLibrary, setPurgingLibrary] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<StorageProjectUsageDto | null>(null);
  const [deletingProject, setDeletingProject] = useState(false);

  const statusLimitBytes = status?.limitBytes;
  useEffect(() => {
    if (statusLimitBytes === undefined) return;
    setLimitDraft(formatLimitGbDraft(statusLimitBytes));
    setLimitError(null);
  }, [statusLimitBytes]);

  if (isLoading && !status) {
    return (
      <div className="flex items-center gap-2 rounded-xl border border-hair bg-surface p-5 text-caption text-fg2">
        <Loader2 className="size-3.5 animate-spin" strokeWidth={ICON_STROKE} />
        <span>{m["storage.loading"]()}</span>
      </div>
    );
  }

  if (error && !status) {
    return (
      <div
        role="alert"
        className="flex items-center justify-between gap-3 rounded-xl border border-err/40 bg-err/10 p-4 text-caption text-err"
      >
        <span>{error instanceof Error ? error.message : m["storage.loadError"]()}</span>
        <Button type="button" size="sm" variant="outline" onClick={() => void refetch()}>
          {m["storage.retry"]()}
        </Button>
      </div>
    );
  }

  if (!status) return null;

  const isUnlimited = status.limitBytes === null;
  const storedDraft = formatLimitGbDraft(status.limitBytes);
  const dirty = limitDraft !== storedDraft;
  const usageRatio =
    status.limitBytes && status.limitBytes > 0
      ? Math.min(1, status.totalBytes / status.limitBytes)
      : null;
  const usagePct = usageRatio !== null ? Math.round(usageRatio * 100) : null;
  const barColor = status.overBudget
    ? "bg-err"
    : status.belowDiskFloor || (usageRatio !== null && usageRatio >= 0.9)
      ? "bg-warn"
      : "bg-accent";

  const commitBudgetValue = async (nextLimitBytes: number | null) => {
    if (budgetInFlight.current) return;
    if (nextLimitBytes === status.limitBytes) return;
    budgetInFlight.current = true;
    setSavingBudget(true);
    try {
      const updated = await storageSetBudget(nextLimitBytes);
      queryClient.setQueryData(queryKeys.storageStatus, updated);
      await queryClient.invalidateQueries({ queryKey: queryKeys.storageStatus });
      toast.success(m["storage.budgetUpdatedToast"]());
    } catch (err) {
      toast.error(err instanceof Error ? err.message : m["storage.budgetUpdateFailed"]());
      setLimitDraft(storedDraft);
    } finally {
      budgetInFlight.current = false;
      setSavingBudget(false);
    }
  };

  const commitBudgetDraft = async () => {
    const trimmed = limitDraft.trim();
    if (!trimmed) {
      if (isUnlimited) {
        setLimitError(null);
        return;
      }
      await commitBudgetValue(null);
      return;
    }
    const gb = Number(trimmed);
    if (!Number.isFinite(gb) || gb <= 0) {
      setLimitError(m["storage.budgetInvalid"]());
      return;
    }
    setLimitError(null);
    const bytes = Math.round(gb * GIB);
    await commitBudgetValue(bytes);
  };

  const handleCleanCache = async () => {
    if (cleaningCache) return;
    setCleaningCache(true);
    try {
      const res = await storageCleanCache();
      queryClient.setQueryData(queryKeys.storageStatus, res.status);
      await queryClient.invalidateQueries({ queryKey: queryKeys.storageStatus });
      toast.success(m["storage.cacheCleaned"]({ size: formatBytes(res.freedBytes) }));
    } catch (err) {
      toast.error(err instanceof Error ? err.message : m["storage.cacheCleanFailed"]());
    } finally {
      setCleaningCache(false);
    }
  };

  const handlePurgeLibrary = async () => {
    if (purgingLibrary) return;
    setPurgingLibrary(true);
    try {
      const res = await opInvoke(activeProjectId, "library.purge", {});
      const output = (res.output ?? {}) as {
        purged_entries?: number;
        freed_bytes?: number;
      };
      const count = output.purged_entries ?? 0;
      const freed = output.freed_bytes ?? 0;
      setPurgeDialogOpen(false);
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: queryKeys.storageStatus }),
        queryClient.invalidateQueries({ queryKey: GLOBAL_LIBRARY_QUERY_KEY }),
      ]);
      if (count > 0) {
        toast.success(m["storage.purgedToast"]({ count, size: formatBytes(freed) }));
      } else {
        toast.info(m["storage.purgeNothingToast"]());
      }
    } catch (err) {
      toast.error(err instanceof Error ? err.message : m["storage.purgeFailed"]());
    } finally {
      setPurgingLibrary(false);
    }
  };

  const handleDeleteProject = async () => {
    if (!deleteTarget || deletingProject) return;
    const target = deleteTarget;
    setDeletingProject(true);
    try {
      await deleteProject(target.projectId);
      setDeleteTarget(null);
      await queryClient.invalidateQueries({ queryKey: queryKeys.storageStatus });
      toast.success(m["storage.deletedProjectToast"]({ name: target.name }));
    } catch (err) {
      toast.error(err instanceof Error ? err.message : m["storage.deleteProjectFailed"]());
    } finally {
      setDeletingProject(false);
    }
  };

  const historyTotalBytes = status.projects.reduce((sum, p) => sum + p.historyBytes, 0);
  const sortedProjects = [...status.projects].sort((a, b) => b.totalBytes - a.totalBytes);
  const floorPercent = Math.round(status.diskFloorRatio * 100);

  return (
    <div className="space-y-6" data-testid="storage-settings-section">
      {/* Summary & Budget Card */}
      <SettingsCard>
        <div className="space-y-3 px-5 py-4">
          <div className="flex flex-wrap items-baseline justify-between gap-2">
            <div className="text-body-sm font-medium text-fg" data-testid="storage-usage-summary">
              {status.limitBytes !== null
                ? m["storage.usageOfLimit"]({
                    used: formatBytes(status.totalBytes),
                    limit: formatBytes(status.limitBytes),
                  })
                : m["storage.usageSummary"]({ used: formatBytes(status.totalBytes) })}
            </div>
            <div className="flex items-center gap-2 text-caption text-fg2">
              {status.scanRunning && (
                <span className="inline-flex items-center gap-1 text-fg3">
                  <Loader2 className="size-3 animate-spin" strokeWidth={ICON_STROKE} />
                  {m["storage.scanning"]()}
                </span>
              )}
              {isUnlimited ? (
                <ValueChip>{m["storage.unlimited"]()}</ValueChip>
              ) : (
                <span className="font-mono tabular-nums text-fg2">{usagePct}%</span>
              )}
            </div>
          </div>

          {usageRatio !== null && (
            <div
              role="progressbar"
              aria-valuemin={0}
              aria-valuemax={100}
              aria-valuenow={Math.min(100, usagePct ?? 0)}
              className="h-2 w-full overflow-hidden rounded-full bg-tile"
            >
              <div
                className={cn("h-full rounded-full transition-all", barColor)}
                style={{ width: `${Math.min(100, Math.round(usageRatio * 100))}%` }}
              />
            </div>
          )}

          {(status.overBudget || status.belowDiskFloor) && (
            <div
              role="alert"
              data-testid="storage-settings-alert"
              className="flex items-start gap-2 rounded-lg border border-err/40 bg-err/10 px-3 py-2 text-caption text-err"
            >
              <CircleAlert className="mt-0.5 size-3.5 shrink-0" strokeWidth={ICON_STROKE} />
              <div className="space-y-0.5">
                {status.overBudget && <div>{m["storage.overBudgetAlert"]()}</div>}
                {status.belowDiskFloor && <div>{m["storage.belowDiskFloorAlert"]()}</div>}
              </div>
            </div>
          )}
        </div>

        <SettingRow
          labelId="setting-storage-budget-label"
          label={m["storage.budgetLabel"]()}
          description={m["storage.budgetDescription"]()}
        >
          <div className="flex flex-col items-end gap-1.5">
            <div className="flex items-center gap-2">
              <div className="relative flex items-center">
                <Input
                  type="number"
                  min="0.1"
                  step="any"
                  aria-labelledby="setting-storage-budget-label"
                  aria-invalid={limitError ? true : undefined}
                  data-settings-escape-reverts={dirty || limitError ? "true" : undefined}
                  data-testid="storage-budget-input"
                  placeholder={m["storage.unlimited"]()}
                  value={limitDraft}
                  readOnly={savingBudget}
                  onChange={(e) => {
                    setLimitDraft(e.target.value);
                    if (limitError) setLimitError(null);
                  }}
                  onBlur={() => void commitBudgetDraft()}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      e.preventDefault();
                      void commitBudgetDraft();
                    } else if (e.key === "Escape" && (dirty || limitError)) {
                      setLimitDraft(storedDraft);
                      setLimitError(null);
                    }
                  }}
                  className="h-8 w-28 pr-9 text-right font-mono text-ui"
                />
                <span className="pointer-events-none absolute right-2.5 text-caption text-fg3">
                  {m["storage.budgetLimitUnit"]()}
                </span>
              </div>
              <Button
                type="button"
                size="sm"
                variant={isUnlimited ? "secondary" : "outline"}
                disabled={isUnlimited || savingBudget}
                onClick={() => {
                  setLimitError(null);
                  setLimitDraft("");
                  void commitBudgetValue(null);
                }}
                data-testid="storage-budget-unlimited"
              >
                {m["storage.unlimited"]()}
              </Button>
            </div>
            {limitError && (
              <p role="alert" className="text-caption text-err">
                {limitError}
              </p>
            )}
          </div>
        </SettingRow>

        <SettingRow
          labelId="setting-storage-floor-label"
          label={m["storage.diskFloorLabel"]({ percent: floorPercent })}
          description={m["storage.diskFloorThreshold"]({
            floor: status.diskFloorBytes !== null ? formatBytes(status.diskFloorBytes) : "—",
          })}
        >
          <ValueChip className={cn(status.belowDiskFloor && "border-err/40 text-err")}>
            {status.diskAvailableBytes !== null && status.diskTotalBytes !== null
              ? m["storage.diskAvailable"]({
                  available: formatBytes(status.diskAvailableBytes),
                  total: formatBytes(status.diskTotalBytes),
                })
              : "—"}
          </ValueChip>
        </SettingRow>
      </SettingsCard>

      {/* Category Breakdown */}
      <div className="space-y-2">
        <h3 className="text-body-sm font-medium text-fg">{m["storage.breakdownTitle"]()}</h3>
        <SettingsCard>
          <SettingRow
            labelId="storage-cat-cache"
            label={m["storage.cacheLabel"]()}
            description={m["storage.cacheDesc"]()}
          >
            <div className="flex items-center gap-2.5">
              <ValueChip data-testid="storage-cache-size">
                {formatBytes(status.categories.cacheBytes)}
              </ValueChip>
              <Button
                type="button"
                size="sm"
                variant="outline"
                disabled={cleaningCache || status.categories.cacheBytes === 0}
                onClick={() => void handleCleanCache()}
                data-testid="storage-clean-cache-btn"
              >
                {cleaningCache ? m["storage.cleaning"]() : m["storage.cleanNow"]()}
              </Button>
            </div>
          </SettingRow>

          <SettingRow
            labelId="storage-cat-library"
            label={m["storage.libraryLabel"]()}
            description={
              status.categories.libraryRemovedBytes > 0
                ? `${m["storage.libraryDesc"]()} · ${m["storage.libraryRemovedHint"]({
                    size: formatBytes(status.categories.libraryRemovedBytes),
                  })}`
                : m["storage.libraryDesc"]()
            }
          >
            <div className="flex flex-wrap items-center justify-end gap-2">
              <ValueChip data-testid="storage-library-size">
                {formatBytes(status.categories.libraryBytes)}
              </ValueChip>
              <Button
                type="button"
                size="sm"
                variant="outline"
                onClick={() => {
                  closeSettings();
                  void navigate({ to: "/assets", search: { sort: "size" } });
                }}
                data-testid="storage-library-view-by-size"
              >
                {m["storage.viewBySize"]()}
              </Button>
              <Button
                type="button"
                size="sm"
                variant="outline"
                disabled={status.categories.libraryRemovedBytes === 0 || purgingLibrary}
                onClick={() => setPurgeDialogOpen(true)}
                data-testid="storage-library-purge-btn"
              >
                {m["storage.purgeRemoved"]()}
              </Button>
            </div>
          </SettingRow>

          <SettingRow
            labelId="storage-cat-models"
            label={m["storage.modelsLabel"]()}
            description={m["storage.modelsDesc"]()}
          >
            <div className="flex items-center gap-2.5">
              <ValueChip data-testid="storage-models-size">
                {formatBytes(status.categories.modelsBytes)}
              </ValueChip>
              <Button
                type="button"
                size="sm"
                variant="outline"
                onClick={() => {
                  closeSettings();
                  void navigate({ to: "/models" });
                }}
                data-testid="storage-manage-models"
              >
                {m["storage.manageModels"]()}
              </Button>
            </div>
          </SettingRow>

          <SettingRow
            labelId="storage-cat-workers"
            label={m["storage.workersLabel"]()}
            description={m["storage.workersDesc"]()}
          >
            <div className="flex items-center gap-2.5">
              <ValueChip data-testid="storage-workers-size">
                {formatBytes(status.categories.workersBytes)}
              </ValueChip>
              <Button
                type="button"
                size="sm"
                variant="outline"
                onClick={() => setSettingsSection("mods")}
                data-testid="storage-manage-workers"
              >
                {m["storage.manageModules"]()}
              </Button>
            </div>
          </SettingRow>

          <SettingRow
            labelId="storage-cat-mods"
            label={m["storage.modsLabel"]()}
            description={m["storage.modsDesc"]()}
          >
            <div className="flex items-center gap-2.5">
              <ValueChip data-testid="storage-mods-size">
                {formatBytes(status.categories.modsBytes)}
              </ValueChip>
              <Button
                type="button"
                size="sm"
                variant="outline"
                onClick={() => setSettingsSection("mods")}
                data-testid="storage-manage-mods"
              >
                {m["storage.manageModules"]()}
              </Button>
            </div>
          </SettingRow>

          <SettingRow
            labelId="storage-cat-history"
            label={m["storage.fileHistoryLabel"]()}
            description={m["storage.fileHistoryDesc"]()}
          >
            <div className="flex items-center gap-2">
              <ValueChip className="opacity-70">{formatBytes(historyTotalBytes)}</ValueChip>
              <span className="text-caption text-fg3">{m["storage.fileHistoryDisabled"]()}</span>
            </div>
          </SettingRow>
        </SettingsCard>
      </div>

      {/* Per-Project Breakdown */}
      <div className="space-y-2">
        <div className="flex items-baseline justify-between gap-2">
          <h3 className="text-body-sm font-medium text-fg">{m["storage.projectsTitle"]()}</h3>
          <span className="text-caption text-fg2">
            {m["storage.projectsSubtitle"]({
              size: formatBytes(status.categories.projectsTotalBytes),
            })}
          </span>
        </div>

        {sortedProjects.length === 0 ? (
          <div className="rounded-xl border border-hair bg-surface px-5 py-4 text-caption text-fg2">
            {m["storage.projectsEmpty"]()}
          </div>
        ) : (
          <SettingsCard>
            {sortedProjects.map((project) => (
              <SettingRow
                key={project.projectId}
                labelId={`storage-proj-${project.projectId}`}
                label={project.name}
                description={m["storage.projectBreakdown"]({
                  assets: formatBytes(project.assetsBytes),
                  history: formatBytes(project.historyBytes),
                })}
              >
                <div className="flex items-center gap-2.5">
                  <ValueChip>{formatBytes(project.totalBytes)}</ValueChip>
                  <Button
                    type="button"
                    size="sm"
                    variant="ghost"
                    onClick={() => setDeleteTarget(project)}
                    className="gap-1 text-fg2 hover:text-err"
                    data-testid={`storage-delete-project-${project.projectId}`}
                  >
                    <Trash2 className="size-3.5" strokeWidth={ICON_STROKE} />
                    {m["storage.deleteProject"]()}
                  </Button>
                </div>
              </SettingRow>
            ))}
          </SettingsCard>
        )}
      </div>

      {/* Purge Removed Confirmation Dialog */}
      <Dialog
        open={purgeDialogOpen}
        onOpenChange={(next) => !next && !purgingLibrary && setPurgeDialogOpen(false)}
      >
        <DialogContent
          showCloseButton={!purgingLibrary}
          data-testid="storage-purge-confirm-dialog"
          className="sm:max-w-md"
        >
          <DialogHeader>
            <DialogTitle>{m["storage.purgeConfirmTitle"]()}</DialogTitle>
            <DialogDescription>
              {m["storage.purgeConfirmDesc"]({
                size: formatBytes(status.categories.libraryRemovedBytes),
              })}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              disabled={purgingLibrary}
              onClick={() => setPurgeDialogOpen(false)}
            >
              {m["common.cancel"]()}
            </Button>
            <Button
              type="button"
              variant="destructive"
              disabled={purgingLibrary}
              onClick={() => void handlePurgeLibrary()}
              data-testid="storage-purge-confirm-btn"
            >
              {purgingLibrary ? m["storage.purging"]() : m["storage.purgeConfirmButton"]()}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Delete Project Confirmation Dialog */}
      <Dialog
        open={deleteTarget !== null}
        onOpenChange={(next) => !next && !deletingProject && setDeleteTarget(null)}
      >
        <DialogContent
          showCloseButton={!deletingProject}
          data-testid="storage-delete-project-dialog"
          className="sm:max-w-md"
        >
          <DialogHeader>
            <DialogTitle>
              {deleteTarget
                ? m["storage.deleteProjectConfirmTitle"]({ name: deleteTarget.name })
                : ""}
            </DialogTitle>
            <DialogDescription>
              {deleteTarget
                ? m["storage.deleteProjectConfirmDesc"]({
                    size: formatBytes(deleteTarget.totalBytes),
                  })
                : ""}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              disabled={deletingProject}
              onClick={() => setDeleteTarget(null)}
            >
              {m["common.cancel"]()}
            </Button>
            <Button
              type="button"
              variant="destructive"
              disabled={deletingProject}
              onClick={() => void handleDeleteProject()}
              data-testid="storage-delete-project-confirm-btn"
            >
              {deletingProject
                ? m["storage.deleting"]()
                : m["storage.deleteProjectConfirmButton"]()}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
