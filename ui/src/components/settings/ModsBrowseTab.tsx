import { Download, RefreshCw, Search, ShieldCheck, X } from "lucide-react";
import * as m from "$paraglide/messages";
import { ICON_STROKE } from "@/design/tokens";
import type {
  ModBrowseResponseDto,
  ModDownloadTaskDto,
  ModSourcesListDto,
  RemoteModCatalogItemDto,
} from "@/ipc/schemas";
import { Button } from "@/components/ui/button";
import { SettingsCard } from "./SettingRow";

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export function ModsBrowseTab({
  sources,
  browseData,
  isLoading,
  isRefreshing,
  errorMessage,
  selectedSourceId,
  onSelectSourceId,
  searchQuery,
  onSearchQueryChange,
  onRefresh,
  downloads,
  onStartDownload,
  onCancelDownload,
  onReviewDownload,
}: {
  sources: ModSourcesListDto | undefined;
  browseData: ModBrowseResponseDto | undefined;
  isLoading: boolean;
  isRefreshing: boolean;
  errorMessage: string | null;
  selectedSourceId: string;
  onSelectSourceId: (sourceId: string) => void;
  searchQuery: string;
  onSearchQueryChange: (q: string) => void;
  onRefresh: () => void;
  downloads: ModDownloadTaskDto[];
  onStartDownload: (item: RemoteModCatalogItemDto) => void;
  onCancelDownload: (taskId: string) => void;
  onReviewDownload: (task: ModDownloadTaskDto) => void;
}) {
  const queryLower = searchQuery.trim().toLowerCase();
  const items = (browseData?.items ?? []).filter((item) => {
    if (!queryLower) return true;
    return (
      item.name.toLowerCase().includes(queryLower) ||
      item.id.toLowerCase().includes(queryLower) ||
      (item.description?.toLowerCase().includes(queryLower) ?? false) ||
      item.capabilities.some((cap) => cap.toLowerCase().includes(queryLower))
    );
  });

  const activeDownloads = downloads.filter(
    (d) =>
      d.status === "downloading" || d.status === "verifying" || d.status === "awaiting_consent",
  );
  const failedOutcomes = (browseData?.outcomes ?? []).filter((o) => !o.ok && o.error);

  return (
    <div className="space-y-4">
      {/* Filter & Search Bar */}
      <div className="flex flex-wrap items-center gap-2">
        <div className="relative min-w-[180px] flex-1">
          <Search
            className="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-fg2"
            aria-hidden="true"
            strokeWidth={ICON_STROKE}
          />
          <input
            type="search"
            value={searchQuery}
            onChange={(e) => onSearchQueryChange(e.target.value)}
            placeholder={m["mods.browseSearchPlaceholder"]()}
            aria-label={m["mods.browseSearchPlaceholder"]()}
            className="h-8 w-full rounded-md border border-hair bg-surface pr-3 pl-8 text-caption text-fg placeholder:text-fg2 focus:border-accent focus:outline-none"
          />
        </div>

        <select
          aria-label="Mod registry source filter"
          value={selectedSourceId}
          onChange={(e) => onSelectSourceId(e.target.value)}
          className="h-8 rounded-md border border-hair bg-surface px-2.5 text-caption text-fg focus:border-accent focus:outline-none"
        >
          <option value="all">{m["mods.browseAllSources"]()}</option>
          <option value="official">{m["mods.sourcesOfficialTitle"]()}</option>
          {(sources?.third_party ?? [])
            .filter((s) => s.enabled)
            .map((src) => (
              <option key={src.id} value={src.id}>
                {src.name}
              </option>
            ))}
        </select>

        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={onRefresh}
          disabled={isRefreshing}
          className="h-8 gap-1.5 text-caption"
        >
          <RefreshCw
            className={`size-3.5 ${isRefreshing ? "animate-spin" : ""}`}
            aria-hidden="true"
            strokeWidth={ICON_STROKE}
          />
          {m["mods.browseRefresh"]()}
        </Button>
      </div>

      {/* Active Downloads Panel */}
      {activeDownloads.length > 0 && (
        <SettingsCard>
          <div className="px-5 py-3">
            <div className="mb-2 text-caption font-medium text-fg">
              {m["mods.downloadActiveTitle"]()}
            </div>
            <div className="space-y-2.5">
              {activeDownloads.map((task) => {
                const pct =
                  task.total_bytes && task.total_bytes > 0
                    ? Math.min(100, Math.round((task.downloaded_bytes / task.total_bytes) * 100))
                    : null;
                return (
                  <div
                    key={task.task_id}
                    data-testid={`mod-download-${task.mod_id}`}
                    className="rounded-lg border border-hair-soft bg-ground p-3 text-caption"
                  >
                    <div className="flex flex-wrap items-center justify-between gap-2">
                      <div className="font-medium text-fg">
                        {task.mod_id} · v{task.version}
                      </div>
                      <div className="flex items-center gap-2">
                        <span className="text-fg2">
                          {task.status === "verifying"
                            ? m["mods.downloadVerifying"]()
                            : task.status === "awaiting_consent"
                              ? m["mods.downloadAwaitingConsent"]()
                              : m["mods.downloadDownloading"]()}
                          {pct !== null ? ` (${pct}%)` : ""}
                        </span>
                        {task.status === "awaiting_consent" && (
                          <Button
                            type="button"
                            size="sm"
                            onClick={() => onReviewDownload(task)}
                            className="h-6 px-2 text-[11px]"
                          >
                            {m["mods.downloadReviewButton"]()}
                          </Button>
                        )}
                        <Button
                          type="button"
                          variant="ghost"
                          size="sm"
                          onClick={() => onCancelDownload(task.task_id)}
                          className="h-6 gap-1 px-2 text-[11px] text-fg2 hover:text-err"
                        >
                          <X className="size-3" aria-hidden="true" strokeWidth={ICON_STROKE} />
                          {m["mods.downloadCancel"]()}
                        </Button>
                      </div>
                    </div>
                    <div className="mt-2 h-1.5 w-full overflow-hidden rounded-full bg-hair">
                      <div
                        className="h-full bg-accent transition-all"
                        style={{ width: `${pct ?? 35}%` }}
                      />
                    </div>
                    <div className="mt-1 font-mono text-[11px] text-fg2">
                      {formatBytes(task.downloaded_bytes)}
                      {task.total_bytes ? ` / ${formatBytes(task.total_bytes)}` : ""}
                    </div>
                  </div>
                );
              })}
            </div>
          </div>
        </SettingsCard>
      )}

      {/* Per-source fetch warnings */}
      {failedOutcomes.map((outcome) => (
        <div
          key={outcome.source_id}
          role="alert"
          className="rounded-lg border border-warn/40 bg-warn/10 px-4 py-2.5 text-caption text-fg"
        >
          <span className="font-medium">{outcome.source_name}: </span>
          <span className="text-fg2">{outcome.error}</span>
        </div>
      ))}

      {errorMessage && (
        <div
          role="alert"
          className="rounded-xl border border-err/40 bg-err/10 p-4 text-caption text-err"
        >
          {errorMessage}
        </div>
      )}

      {/* Catalog List */}
      {isLoading ? (
        <div className="rounded-xl border border-hair bg-surface p-5 text-caption text-fg2">
          {m["mods.browseLoading"]()}
        </div>
      ) : items.length === 0 ? (
        <div className="rounded-xl border border-hair bg-surface p-5 text-caption text-fg2">
          {m["mods.browseEmpty"]()}
        </div>
      ) : (
        <div className="space-y-3">
          {items.map((item) => {
            const inFlight = downloads.some(
              (d) =>
                d.mod_id === item.id &&
                d.source_id === item.source_id &&
                (d.status === "downloading" ||
                  d.status === "verifying" ||
                  d.status === "awaiting_consent"),
            );
            const alreadyCurrent =
              item.installed_version === item.version && !item.update_available;
            const disabled =
              !item.compatible || Boolean(item.source_conflict) || alreadyCurrent || inFlight;

            let actionLabel = m["mods.browseInstall"]();
            if (!item.compatible) {
              actionLabel = m["mods.browseIncompatible"]();
            } else if (alreadyCurrent) {
              actionLabel = m["mods.browseInstalled"]();
            } else if (item.update_available) {
              actionLabel = m["mods.browseUpgrade"]();
            }

            return (
              <div
                key={`${item.source_id}:${item.id}:${item.version}`}
                data-testid={`browse-mod-${item.id}`}
                className="rounded-xl border border-hair bg-surface px-5 py-3.5"
              >
                <div className="flex flex-wrap items-start justify-between gap-3">
                  <div className="min-w-0 flex-1 space-y-1">
                    <div className="flex flex-wrap items-center gap-2">
                      <span className="text-body-sm font-medium text-fg">{item.name}</span>
                      <span className="font-mono text-caption text-fg2">
                        {item.id} · v{item.version}
                      </span>
                      <span className="inline-flex items-center gap-1 rounded bg-tile px-1.5 py-0.5 text-[11px] font-medium text-fg2">
                        {item.source_kind === "official" && (
                          <ShieldCheck
                            className="size-3 text-accent"
                            aria-hidden="true"
                            strokeWidth={ICON_STROKE}
                          />
                        )}
                        {item.source_label}
                      </span>
                      <span className="rounded bg-tile px-1.5 py-0.5 font-mono text-[11px] text-fg2">
                        {item.target}
                      </span>
                    </div>

                    {item.description && (
                      <p className="text-caption text-fg2">{item.description}</p>
                    )}

                    <div className="flex flex-wrap items-center gap-1.5 pt-0.5 text-[11px]">
                      {item.capabilities.map((cap) => (
                        <span
                          key={cap}
                          className="rounded border border-hair-soft bg-ground px-1.5 py-0.5 text-fg2"
                        >
                          {cap}
                        </span>
                      ))}
                      {item.permissions.host_paths && (
                        <span className="rounded bg-warn/10 px-1.5 py-0.5 text-warn">
                          {m["mods.riskHostPath"]()}
                        </span>
                      )}
                      {item.permissions.network && (
                        <span className="rounded bg-warn/10 px-1.5 py-0.5 text-warn">
                          {m["mods.permNetwork"]()}
                        </span>
                      )}
                      {item.source_conflict && (
                        <span className="text-err">{item.source_conflict}</span>
                      )}
                      {!item.compatible && item.compatibility_reason && (
                        <span className="text-err">{item.compatibility_reason}</span>
                      )}
                    </div>
                  </div>

                  <Button
                    type="button"
                    variant={item.update_available || !alreadyCurrent ? "default" : "outline"}
                    size="sm"
                    disabled={disabled}
                    onClick={() => onStartDownload(item)}
                    className="shrink-0 gap-1.5 text-caption"
                  >
                    <Download className="size-3.5" aria-hidden="true" strokeWidth={ICON_STROKE} />
                    {actionLabel}
                  </Button>
                </div>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
