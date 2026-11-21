import { useEffect, useRef, useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { useQueryClient } from "@tanstack/react-query";
import { ChevronRight, CircleAlert, HardDrive, Loader2, RefreshCw, Trash2 } from "lucide-react";
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
import { SettingsCard } from "./SettingRow";

const GIB = 1024 * 1024 * 1024;
const BUDGET_PRESETS_GB = [20, 50, 100] as const;
const INITIAL_PROJECTS_VISIBLE = 5;

function formatLimitGbDraft(limitBytes: number | null): string {
  if (limitBytes === null) return "";
  const gb = limitBytes / GIB;
  if (Number.isInteger(gb)) return String(gb);
  if (gb >= 0.01) return String(Number(gb.toFixed(2)));
  return Number(gb.toPrecision(2)).toString();
}

function SizeReadout({
  bytes,
  className,
  "data-testid": testId,
}: {
  bytes: number;
  className?: string;
  "data-testid"?: string;
}) {
  return (
    <span
      data-testid={testId}
      className={cn(
        "font-mono text-ui tabular-nums",
        bytes === 0 ? "text-fg3" : "font-medium text-fg",
        className,
      )}
    >
      {formatBytes(bytes)}
    </span>
  );
}

/** Settings -> App -> Storage management section. */
export function StorageSettingsSection() {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const activeProjectId = useProjectStore((s) => s.activeProjectId);
  const deleteProject = useProjectStore((s) => s.deleteProject);
  const closeSettings = useUIStore((s) => s.closeSettings);

  const { data: status, isLoading, isFetching, error, refetch } = useStorageStatus();

  const [limitDraft, setLimitDraft] = useState("");
  const [limitError, setLimitError] = useState<string | null>(null);
  const [savingBudget, setSavingBudget] = useState(false);
  const budgetInFlight = useRef(false);

  const [cleaningCache, setCleaningCache] = useState(false);
  const [purgeDialogOpen, setPurgeDialogOpen] = useState(false);
  const [purgingLibrary, setPurgingLibrary] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<StorageProjectUsageDto | null>(null);
  const [deletingProject, setDeletingProject] = useState(false);
  const [showAllProjects, setShowAllProjects] = useState(false);

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

  const modulesBytes = status.categories.workersBytes + status.categories.modsBytes;
  const segments = [
    {
      key: "models",
      label: m["storage.modelsLabel"](),
      bytes: status.categories.modelsBytes,
      colorClass: "bg-accent",
    },
    {
      key: "library",
      label: m["storage.libraryLabel"](),
      bytes: status.categories.libraryBytes,
      colorClass: "bg-info",
    },
    {
      key: "projects",
      label: m["storage.projectsTitle"](),
      bytes: status.categories.projectsTotalBytes,
      colorClass: "bg-warn",
    },
    {
      key: "modules",
      label: m["storage.modulesLabel"](),
      bytes: modulesBytes,
      colorClass: "bg-fg2/70",
    },
    {
      key: "cache",
      label: m["storage.cacheLabel"](),
      bytes: status.categories.cacheBytes,
      colorClass: "bg-fg4",
    },
  ];
  const nonZeroSegments = segments.filter((seg) => seg.bytes > 0);
  const totalSegmentBytes = nonZeroSegments.reduce((sum, seg) => sum + seg.bytes, 0);
  const barDenominator =
    status.limitBytes !== null && status.limitBytes > 0
      ? Math.max(status.limitBytes, totalSegmentBytes)
      : totalSegmentBytes;

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

  const handleSelectPreset = (gb: number | null) => {
    setLimitError(null);
    setLimitDraft(gb === null ? "" : String(gb));
    void commitBudgetValue(gb === null ? null : gb * GIB);
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
  const visibleProjects = showAllProjects
    ? sortedProjects
    : sortedProjects.slice(0, INITIAL_PROJECTS_VISIBLE);
  const hasHiddenProjects = sortedProjects.length > INITIAL_PROJECTS_VISIBLE;
  const floorPercent = Math.round(status.diskFloorRatio * 100);

  return (
    <div className="space-y-6" data-testid="storage-settings-section">
      {/* Overview & Budget Card */}
      <SettingsCard>
        <div className="space-y-4 px-5 py-4">
          {/* Top Metric Row */}
          <div className="flex items-start justify-between gap-3">
            <div
              className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5"
              data-testid="storage-usage-summary"
            >
              <span className="text-display-sm font-semibold tabular-nums text-fg">
                {formatBytes(status.totalBytes)}
              </span>
              <span className="text-body-sm text-fg2">
                {status.limitBytes !== null
                  ? m["storage.ofLimit"]({ limit: formatBytes(status.limitBytes) })
                  : m["storage.usedLabel"]()}
              </span>
            </div>

            <div className="flex items-center gap-2">
              {status.scanRunning && (
                <span className="inline-flex items-center gap-1 text-caption text-fg3">
                  <Loader2 className="size-3 animate-spin" strokeWidth={ICON_STROKE} />
                  {m["storage.scanning"]()}
                </span>
              )}
              {isUnlimited ? (
                <span className="rounded-md bg-tile px-2 py-0.5 text-caption font-medium text-fg2">
                  {m["storage.unlimited"]()}
                </span>
              ) : (
                <span
                  className={cn(
                    "rounded-md px-2 py-0.5 font-mono text-caption font-medium tabular-nums",
                    status.overBudget
                      ? "bg-err/12 text-err"
                      : usageRatio !== null && usageRatio >= 0.9
                        ? "bg-warn/12 text-warn"
                        : "bg-tile text-fg2",
                  )}
                >
                  {usagePct}%
                </span>
              )}
              <Button
                type="button"
                variant="ghost"
                size="icon-xs"
                disabled={isFetching}
                aria-label={m["storage.refresh"]()}
                title={m["storage.refresh"]()}
                onClick={() => void refetch()}
                className="text-fg3 hover:text-fg"
                data-testid="storage-refresh-btn"
              >
                <RefreshCw
                  className={cn("size-3.5", (isFetching || status.scanRunning) && "animate-spin")}
                  strokeWidth={ICON_STROKE}
                />
              </Button>
            </div>
          </div>

          {/* Segmented Usage Bar */}
          <div className="space-y-2.5">
            <div
              role={usageRatio !== null ? "progressbar" : undefined}
              aria-valuemin={usageRatio !== null ? 0 : undefined}
              aria-valuemax={usageRatio !== null ? 100 : undefined}
              aria-valuenow={usageRatio !== null ? Math.min(100, usagePct ?? 0) : undefined}
              data-testid="storage-segmented-bar"
              className={cn(
                "flex h-2 w-full overflow-hidden rounded-full bg-tile",
                nonZeroSegments.length > 1 && "gap-[2px]",
                status.overBudget && "ring-1 ring-err/45",
              )}
            >
              {barDenominator > 0 &&
                nonZeroSegments.map((seg) => {
                  const widthPct = (seg.bytes / barDenominator) * 100;
                  return (
                    <div
                      key={seg.key}
                      title={`${seg.label}: ${formatBytes(seg.bytes)}`}
                      className={cn(
                        "h-full min-w-[4px] first:rounded-l-full last:rounded-r-full transition-all",
                        seg.colorClass,
                      )}
                      style={{ width: `${widthPct}%` }}
                    />
                  );
                })}
            </div>

            {nonZeroSegments.length > 0 && (
              <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-caption text-fg2">
                {nonZeroSegments.map((seg) => (
                  <div key={seg.key} className="inline-flex items-center gap-1.5">
                    <span className={cn("size-2 shrink-0 rounded-full", seg.colorClass)} />
                    <span>{seg.label}</span>
                    <span className="font-mono tabular-nums text-fg3">
                      {formatBytes(seg.bytes)}
                    </span>
                  </div>
                ))}
              </div>
            )}
          </div>

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

        {/* Budget Limit Control */}
        <div className="flex flex-wrap items-center justify-between gap-4 px-5 py-3.5">
          <div className="min-w-0 flex-1">
            <div id="setting-storage-budget-label" className="text-body-sm font-medium text-fg">
              {m["storage.budgetLabel"]()}
            </div>
            <div className="mt-0.5 text-caption leading-relaxed text-fg2">
              {m["storage.budgetDescription"]()}
            </div>
          </div>

          <div className="flex flex-col items-end gap-1.5">
            <div className="flex flex-wrap items-center justify-end gap-1.5">
              <div className="inline-flex items-center rounded-lg border border-hair bg-app p-0.5">
                <button
                  type="button"
                  disabled={savingBudget}
                  onClick={() => handleSelectPreset(null)}
                  data-testid="storage-budget-unlimited"
                  className={cn(
                    "h-7 cursor-pointer rounded-md px-2.5 text-caption transition-colors disabled:cursor-default disabled:opacity-50",
                    isUnlimited
                      ? "bg-surface font-medium text-fg shadow-xs"
                      : "text-fg2 hover:text-fg",
                  )}
                >
                  {m["storage.unlimited"]()}
                </button>
                {BUDGET_PRESETS_GB.map((gb) => {
                  const active = status.limitBytes === gb * GIB;
                  return (
                    <button
                      key={gb}
                      type="button"
                      disabled={savingBudget}
                      onClick={() => handleSelectPreset(gb)}
                      data-testid={`storage-budget-preset-${gb}`}
                      className={cn(
                        "h-7 cursor-pointer rounded-md px-2 font-mono text-caption tabular-nums transition-colors disabled:cursor-default disabled:opacity-50",
                        active
                          ? "bg-surface font-medium text-fg shadow-xs"
                          : "text-fg2 hover:text-fg",
                      )}
                    >
                      {gb} {m["storage.budgetLimitUnit"]()}
                    </button>
                  );
                })}
              </div>

              <div className="relative flex items-center">
                <Input
                  type="text"
                  inputMode="decimal"
                  aria-labelledby="setting-storage-budget-label"
                  aria-invalid={limitError ? true : undefined}
                  data-settings-escape-reverts={dirty || limitError ? "true" : undefined}
                  data-testid="storage-budget-input"
                  placeholder={m["storage.budgetCustomPlaceholder"]()}
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
                  className="h-8 w-28 pr-9 text-right font-mono text-ui tabular-nums"
                />
                <span className="pointer-events-none absolute right-2.5 text-caption text-fg3">
                  {m["storage.budgetLimitUnit"]()}
                </span>
              </div>
            </div>
            {limitError && (
              <p role="alert" className="text-caption text-err">
                {limitError}
              </p>
            )}
          </div>
        </div>

        {/* Disk Capacity & Safety Floor Footer */}
        <div className="flex flex-wrap items-center justify-between gap-2 bg-app/50 px-5 py-2.5 text-caption text-fg2">
          <div className="inline-flex items-center gap-1.5">
            <HardDrive
              className={cn("size-3.5 shrink-0", status.belowDiskFloor ? "text-err" : "text-fg3")}
              strokeWidth={ICON_STROKE}
            />
            <span className={cn("tabular-nums", status.belowDiskFloor && "font-medium text-err")}>
              {status.diskAvailableBytes !== null && status.diskTotalBytes !== null
                ? m["storage.diskFooter"]({
                    available: formatBytes(status.diskAvailableBytes),
                    total: formatBytes(status.diskTotalBytes),
                  })
                : "—"}
            </span>
          </div>
          <span className="tabular-nums text-fg3">
            {m["storage.diskFloorCompact"]({
              percent: floorPercent,
              floor: status.diskFloorBytes !== null ? formatBytes(status.diskFloorBytes) : "—",
            })}
          </span>
        </div>
      </SettingsCard>

      {/* Category Breakdown */}
      <div className="space-y-2">
        <h3 className="text-body-sm font-medium text-fg">{m["storage.breakdownTitle"]()}</h3>
        <SettingsCard>
          {/* Reclaimable Cache */}
          <div className="flex items-center justify-between gap-4 px-5 py-3.5">
            <div className="flex min-w-0 flex-1 items-start gap-2.5">
              <span className="mt-1.5 size-2 shrink-0 rounded-full bg-fg4" />
              <div className="min-w-0">
                <div className="text-body-sm font-medium text-fg">{m["storage.cacheLabel"]()}</div>
                <div className="mt-0.5 text-caption leading-relaxed text-fg2">
                  {m["storage.cacheDesc"]()}
                </div>
              </div>
            </div>
            <div className="flex shrink-0 items-center gap-3">
              <SizeReadout bytes={status.categories.cacheBytes} data-testid="storage-cache-size" />
              <div className="flex min-w-[136px] justify-end">
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
            </div>
          </div>

          {/* Global Assets (+ conditional removed items purge strip) */}
          <div>
            <div className="flex items-center justify-between gap-4 px-5 py-3.5">
              <div className="flex min-w-0 flex-1 items-start gap-2.5">
                <span className="mt-1.5 size-2 shrink-0 rounded-full bg-info" />
                <div className="min-w-0">
                  <div className="text-body-sm font-medium text-fg">
                    {m["storage.libraryLabel"]()}
                  </div>
                  <div className="mt-0.5 text-caption leading-relaxed text-fg2">
                    {m["storage.libraryDesc"]()}
                  </div>
                </div>
              </div>
              <div className="flex shrink-0 items-center gap-3">
                <SizeReadout
                  bytes={status.categories.libraryBytes}
                  data-testid="storage-library-size"
                />
                <div className="flex min-w-[136px] justify-end">
                  <Button
                    type="button"
                    size="sm"
                    variant="ghost"
                    onClick={() => {
                      closeSettings();
                      void navigate({ to: "/assets", search: { sort: "size" } });
                    }}
                    className="gap-1 text-fg2 hover:text-fg"
                    data-testid="storage-library-view-by-size"
                  >
                    {m["storage.viewBySize"]()}
                    <ChevronRight className="size-3.5" strokeWidth={ICON_STROKE} />
                  </Button>
                </div>
              </div>
            </div>

            {status.categories.libraryRemovedBytes > 0 && (
              <div className="flex flex-wrap items-center justify-between gap-3 border-t border-hair-soft bg-app/50 px-5 py-2.5 pl-9">
                <span className="text-caption text-fg2">
                  {m["storage.libraryRemovedHint"]({
                    size: formatBytes(status.categories.libraryRemovedBytes),
                  })}
                </span>
                <Button
                  type="button"
                  size="sm"
                  variant="outline"
                  disabled={purgingLibrary}
                  onClick={() => setPurgeDialogOpen(true)}
                  className="h-7 border-err/30 text-caption text-err hover:bg-err/10 hover:text-err"
                  data-testid="storage-library-purge-btn"
                >
                  {m["storage.purgeRemoved"]()}
                </Button>
              </div>
            )}
          </div>

          {/* Models */}
          <div className="flex items-center justify-between gap-4 px-5 py-3.5">
            <div className="flex min-w-0 flex-1 items-start gap-2.5">
              <span className="mt-1.5 size-2 shrink-0 rounded-full bg-accent" />
              <div className="min-w-0">
                <div className="text-body-sm font-medium text-fg">{m["storage.modelsLabel"]()}</div>
                <div className="mt-0.5 text-caption leading-relaxed text-fg2">
                  {m["storage.modelsDesc"]()}
                </div>
              </div>
            </div>
            <div className="flex shrink-0 items-center gap-3">
              <SizeReadout
                bytes={status.categories.modelsBytes}
                data-testid="storage-models-size"
              />
              <div className="min-w-[136px]" aria-hidden="true" />
            </div>
          </div>

          {/* Modules (Workers + Managed Mods) */}
          <div className="flex items-center justify-between gap-4 px-5 py-3.5">
            <div className="flex min-w-0 flex-1 items-start gap-2.5">
              <span className="mt-1.5 size-2 shrink-0 rounded-full bg-fg2/70" />
              <div className="min-w-0">
                <div className="text-body-sm font-medium text-fg">
                  {m["storage.modulesLabel"]()}
                </div>
                <div className="mt-0.5 text-caption leading-relaxed text-fg2">
                  <span>{m["storage.workersLabel"]()} </span>
                  <span className="font-mono tabular-nums" data-testid="storage-workers-size">
                    {formatBytes(status.categories.workersBytes)}
                  </span>
                  <span> · {m["storage.modsLabel"]()} </span>
                  <span className="font-mono tabular-nums" data-testid="storage-mods-size">
                    {formatBytes(status.categories.modsBytes)}
                  </span>
                </div>
              </div>
            </div>
            <div className="flex shrink-0 items-center gap-3">
              <SizeReadout bytes={modulesBytes} data-testid="storage-modules-size" />
              <div className="flex min-w-[136px] justify-end">
                <Button
                  type="button"
                  size="sm"
                  variant="ghost"
                  onClick={() => {
                    closeSettings();
                    void navigate({ to: "/mods" });
                  }}
                  className="gap-1 text-fg2 hover:text-fg"
                  data-testid="storage-manage-mods"
                >
                  {m["storage.manageModules"]()}
                  <ChevronRight className="size-3.5" strokeWidth={ICON_STROKE} />
                </Button>
              </div>
            </div>
          </div>

          {historyTotalBytes > 0 && (
            <div className="flex items-center justify-between gap-4 px-5 py-3.5">
              <div className="min-w-0 flex-1 pl-4.5">
                <div className="text-body-sm font-medium text-fg">
                  {m["storage.fileHistoryLabel"]()}
                </div>
                <div className="mt-0.5 text-caption leading-relaxed text-fg2">
                  {m["storage.fileHistoryDesc"]()}
                </div>
              </div>
              <SizeReadout bytes={historyTotalBytes} />
            </div>
          )}
        </SettingsCard>

        {historyTotalBytes === 0 && (
          <p className="px-1 text-caption text-fg3">{m["storage.fileHistoryFootnote"]()}</p>
        )}
      </div>

      {/* Per-Project Breakdown */}
      <div className="space-y-2">
        <div className="flex items-baseline justify-between gap-2">
          <div className="flex items-center gap-2">
            <span className="size-2 shrink-0 rounded-full bg-warn" />
            <h3 className="text-body-sm font-medium text-fg">{m["storage.projectsTitle"]()}</h3>
          </div>
          <span className="font-mono text-caption tabular-nums text-fg2">
            {formatBytes(status.categories.projectsTotalBytes)}
          </span>
        </div>

        {sortedProjects.length === 0 ? (
          <div className="rounded-xl border border-hair bg-surface px-5 py-4 text-caption text-fg2">
            {m["storage.projectsEmpty"]()}
          </div>
        ) : (
          <SettingsCard>
            {visibleProjects.map((project) => {
              const isCurrent = project.projectId === activeProjectId;
              return (
                <div
                  key={project.projectId}
                  className="flex items-center justify-between gap-4 px-5 py-3"
                >
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-2">
                      <span
                        id={`storage-proj-${project.projectId}`}
                        className="truncate text-body-sm font-medium text-fg"
                      >
                        {project.name}
                      </span>
                      {isCurrent && (
                        <span className="shrink-0 rounded bg-accent-tint px-1.5 py-0.5 text-[11px] font-medium text-accent">
                          {m["storage.currentWorkspace"]()}
                        </span>
                      )}
                    </div>
                    <div className="mt-0.5 text-caption text-fg2">
                      {project.historyBytes > 0
                        ? m["storage.projectBreakdown"]({
                            assets: formatBytes(project.assetsBytes),
                            history: formatBytes(project.historyBytes),
                          })
                        : m["storage.projectAssetsOnly"]({
                            assets: formatBytes(project.assetsBytes),
                          })}
                    </div>
                  </div>

                  <div className="flex shrink-0 items-center gap-2">
                    <SizeReadout bytes={project.totalBytes} />
                    <Button
                      type="button"
                      size="icon-xs"
                      variant="ghost"
                      aria-label={`${m["storage.deleteProject"]()} ${project.name}`}
                      title={m["storage.deleteProject"]()}
                      onClick={() => setDeleteTarget(project)}
                      className="text-fg3 hover:bg-err/10 hover:text-err"
                      data-testid={`storage-delete-project-${project.projectId}`}
                    >
                      <Trash2 className="size-3.5" strokeWidth={ICON_STROKE} />
                    </Button>
                  </div>
                </div>
              );
            })}

            {hasHiddenProjects && (
              <button
                type="button"
                onClick={() => setShowAllProjects((prev) => !prev)}
                className="w-full cursor-pointer px-5 py-2.5 text-center text-caption font-medium text-fg2 transition-colors hover:bg-hover hover:text-fg"
                data-testid="storage-toggle-all-projects"
              >
                {showAllProjects
                  ? m["storage.showFewerProjects"]()
                  : m["storage.showAllProjects"]({ count: sortedProjects.length })}
              </button>
            )}
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
