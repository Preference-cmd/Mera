import { TriangleAlert } from "lucide-react";
import * as m from "$paraglide/messages";
import { ICON_STROKE } from "@/design/tokens";
import { formatBytes } from "@/lib/library";
import { useStorageStatus } from "@/hooks/queries";
import type { StoragePreflightWriteResult } from "@/ipc/schemas";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";

export type PendingStoragePreflight = {
  itemLabel: string;
  estimatedBytes: number;
  result: StoragePreflightWriteResult;
};

function preflightReasonDescription(reason: StoragePreflightWriteResult["reason"]): string {
  switch (reason) {
    case "over_budget":
      return m["storage.preflightDescOverBudget"]();
    case "would_exceed_budget":
      return m["storage.preflightDescWouldExceed"]();
    case "below_disk_floor":
      return m["storage.preflightDescBelowFloor"]();
    default:
      return m["storage.preflightDescWouldExceed"]();
  }
}

/**
 * Confirmation dialog shown before a model download, mod install, or large
 * global library import when storage budget or the disk free-space floor
 * would be exceeded.
 */
export function StoragePreflightDialog({
  pending,
  busy = false,
  onCancel,
  onConfirm,
}: {
  pending: PendingStoragePreflight | null;
  busy?: boolean;
  onCancel: () => void;
  onConfirm: () => void | Promise<void>;
}) {
  const { data: status } = useStorageStatus();
  const displayEstimatedBytes =
    pending && pending.estimatedBytes > 0
      ? pending.estimatedBytes
      : pending && status
        ? Math.max(0, pending.result.projectedTotalBytes - status.totalBytes)
        : (pending?.estimatedBytes ?? 0);

  return (
    <Dialog open={pending !== null} onOpenChange={(next) => !next && !busy && onCancel()}>
      <DialogContent
        showCloseButton={!busy}
        data-testid="storage-preflight-dialog"
        className="sm:max-w-md"
      >
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <TriangleAlert className="size-4 shrink-0 text-warn" strokeWidth={ICON_STROKE} />
            <span>{m["storage.preflightTitle"]()}</span>
          </DialogTitle>
          <DialogDescription>
            {pending ? preflightReasonDescription(pending.result.reason) : ""}
          </DialogDescription>
        </DialogHeader>

        {pending && (
          <div className="space-y-2 rounded-lg border border-hair bg-ground px-3.5 py-3 text-caption">
            <div className="flex items-center justify-between gap-3">
              <span className="text-fg2">{m["storage.preflightItemLabel"]()}</span>
              <span
                className="max-w-[220px] truncate font-medium text-fg"
                title={pending.itemLabel}
              >
                {pending.itemLabel}
              </span>
            </div>
            <div className="flex items-center justify-between gap-3">
              <span className="text-fg2">{m["storage.preflightWriteSize"]()}</span>
              <span className="font-mono font-medium text-fg">
                {formatBytes(displayEstimatedBytes)}
              </span>
            </div>
            <div className="flex items-center justify-between gap-3">
              <span className="text-fg2">{m["storage.preflightProjectedTotal"]()}</span>
              <span className="font-mono font-medium text-fg">
                {formatBytes(pending.result.projectedTotalBytes)}
              </span>
            </div>
            <div className="flex items-center justify-between gap-3">
              <span className="text-fg2">{m["storage.preflightBudgetLimit"]()}</span>
              <span className="font-mono text-fg2">
                {pending.result.limitBytes !== null
                  ? formatBytes(pending.result.limitBytes)
                  : m["storage.unlimited"]()}
              </span>
            </div>
            {pending.result.availableBytes !== null && (
              <div className="flex items-center justify-between gap-3">
                <span className="text-fg2">{m["storage.preflightAvailableDisk"]()}</span>
                <span className="font-mono text-fg2">
                  {formatBytes(pending.result.availableBytes)}
                </span>
              </div>
            )}
          </div>
        )}

        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            disabled={busy}
            onClick={onCancel}
            data-testid="storage-preflight-cancel"
          >
            {m["storage.preflightCancel"]()}
          </Button>
          <Button
            type="button"
            variant="destructive"
            disabled={busy}
            onClick={() => void onConfirm()}
            data-testid="storage-preflight-confirm"
          >
            {m["storage.preflightContinue"]()}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
