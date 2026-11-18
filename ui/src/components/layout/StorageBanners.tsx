import { useEffect, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, HardDrive, X } from "lucide-react";
import * as m from "$paraglide/messages";
import { ICON_STROKE } from "@/design/tokens";
import { formatBytes } from "@/lib/library";
import { storageDismissNudge, subscribeEvents } from "@/ipc";
import { queryKeys, useStorageStatus } from "@/hooks/queries";
import { useUIStore } from "@/store/uiStore";
import { Button } from "@/components/ui/button";

/**
 * Slim storage status banners mounted directly below WorkbenchTopBar in normal
 * flex flow so they never collide with the top-center Toast, bottom HUD, or
 * floating SessionPanel.
 */
export function StorageBanners() {
  const queryClient = useQueryClient();
  const openSettings = useUIStore((s) => s.openSettings);
  const { data: status } = useStorageStatus();
  const [dismissing, setDismissing] = useState(false);

  useEffect(() => {
    return subscribeEvents((event) => {
      if (
        (event.type === "document" && event.kind === "library.changed") ||
        (event.type === "mod" && (event.kind.startsWith("mod.") || event.kind.startsWith("aigc.")))
      ) {
        void queryClient.invalidateQueries({ queryKey: queryKeys.storageStatus });
      }
    });
  }, [queryClient]);

  if (!status) return null;

  const showWarning = status.overBudget || status.belowDiskFloor;
  const showNudge = status.showFiftyGbNudge;

  if (!showWarning && !showNudge) return null;

  const floorPercent = Math.round(status.diskFloorRatio * 100);
  const usedLabel = formatBytes(status.totalBytes);
  const limitLabel = status.limitBytes !== null ? formatBytes(status.limitBytes) : "";
  const availableLabel =
    status.diskAvailableBytes !== null ? formatBytes(status.diskAvailableBytes) : "0 B";

  let warningText = "";
  if (status.overBudget && status.belowDiskFloor) {
    warningText = m["storage.bannerBoth"]({
      used: usedLabel,
      limit: limitLabel,
      available: availableLabel,
      percent: floorPercent,
    });
  } else if (status.overBudget) {
    warningText = m["storage.bannerOverBudget"]({
      used: usedLabel,
      limit: limitLabel,
    });
  } else if (status.belowDiskFloor) {
    warningText = m["storage.bannerBelowFloor"]({
      available: availableLabel,
      percent: floorPercent,
    });
  }

  const handleDismissNudge = async () => {
    if (dismissing) return;
    setDismissing(true);
    try {
      const updated = await storageDismissNudge();
      queryClient.setQueryData(queryKeys.storageStatus, updated);
      await queryClient.invalidateQueries({ queryKey: queryKeys.storageStatus });
    } catch {
      // Keep banner dismiss non-fatal; next status refresh reconciles.
    } finally {
      setDismissing(false);
    }
  };

  return (
    <div className="relative z-10 flex shrink-0 flex-col border-b border-hair-soft">
      {showWarning && (
        <div
          role="alert"
          data-testid="storage-warning-banner"
          className="flex items-center justify-between gap-3 bg-err/10 px-4 py-1.5 text-caption text-fg"
        >
          <div className="flex min-w-0 items-center gap-2">
            <AlertTriangle className="size-3.5 shrink-0 text-err" strokeWidth={ICON_STROKE} />
            <span className="truncate font-medium">{warningText}</span>
          </div>
          <Button
            type="button"
            size="sm"
            variant="outline"
            onClick={() => openSettings("storage")}
            className="h-6 shrink-0 border-err/30 bg-surface px-2.5 text-caption text-fg hover:bg-hover"
            data-testid="storage-warning-action"
          >
            {m["storage.bannerAction"]()}
          </Button>
        </div>
      )}

      {showNudge && (
        <div
          role="status"
          data-testid="storage-nudge-banner"
          className="flex items-center justify-between gap-3 border-t border-hair-soft bg-warn/10 px-4 py-1.5 text-caption text-fg first:border-t-0"
        >
          <div className="flex min-w-0 items-center gap-2">
            <HardDrive className="size-3.5 shrink-0 text-warn" strokeWidth={ICON_STROKE} />
            <span className="truncate">{m["storage.nudgeMessage"]({ used: usedLabel })}</span>
          </div>
          <div className="flex shrink-0 items-center gap-1.5">
            <Button
              type="button"
              size="sm"
              variant="outline"
              onClick={() => openSettings("storage")}
              className="h-6 bg-surface px-2.5 text-caption"
              data-testid="storage-nudge-action"
            >
              {m["storage.nudgeAction"]()}
            </Button>
            <button
              type="button"
              disabled={dismissing}
              aria-label={m["storage.nudgeDismiss"]()}
              onClick={() => void handleDismissNudge()}
              data-testid="storage-nudge-dismiss"
              className="flex size-6 cursor-pointer items-center justify-center rounded text-fg2 transition-colors hover:bg-hover hover:text-fg focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent disabled:opacity-50"
            >
              <X className="size-3.5" strokeWidth={ICON_STROKE} />
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
