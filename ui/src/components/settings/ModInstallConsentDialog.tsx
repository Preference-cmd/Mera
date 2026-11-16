import { useState } from "react";
import { CircleAlert, ShieldAlert } from "lucide-react";
import * as m from "$paraglide/messages";
import { ICON_STROKE } from "@/design/tokens";
import type { ModInstallPreviewDto, ModPermissions, ModSourceDto } from "@/ipc/schemas";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";

export type ConsentTitleKind = "install" | "upgrade" | "reload";

function formatSourceSummary(source: ModSourceDto, fallbackPath: string): string {
  switch (source.kind) {
    case "bundled":
      return m["mods.source.bundled"]();
    case "official":
      return `${m["mods.source.official"]()} (${source.catalog_url})`;
    case "third_party_url":
      return `${m["mods.source.thirdPartyUrl"]()} (${source.source_url})`;
    case "local_archive":
      return `${m["mods.source.localArchive"]()} (${source.filename})`;
    case "local_directory":
      return `${m["mods.source.localDirectory"]()} (${source.path || fallbackPath})`;
  }
}

export function DeclaredPermissionsList({ permissions }: { permissions: ModPermissions }) {
  const items: string[] = [];
  if (permissions.host_paths) {
    items.push(m["mods.permHostPaths"]());
  }
  if (permissions.network) {
    items.push(m["mods.permNetwork"]());
  }
  if (permissions.events_subscribe.length > 0) {
    items.push(m["mods.permEventsSubscribe"]({ events: permissions.events_subscribe.join(", ") }));
  }
  if (permissions.events_publish.length > 0) {
    items.push(m["mods.permEventsPublish"]({ events: permissions.events_publish.join(", ") }));
  }
  if (permissions.filesystem_note) {
    items.push(m["mods.permFilesystemNote"]({ note: permissions.filesystem_note }));
  }

  if (items.length === 0) {
    return <p className="text-caption text-fg2">{m["mods.permNone"]()}</p>;
  }

  return (
    <ul className="list-disc space-y-1 pl-4 text-caption text-fg2">
      {items.map((item) => (
        <li key={item}>{item}</li>
      ))}
    </ul>
  );
}

function ConsentDialogBody({
  mode,
  sourcePath,
  preview,
  titleKind,
  busy,
  onClose,
  onConfirm,
}: {
  mode: "archive" | "directory";
  sourcePath: string;
  preview: ModInstallPreviewDto;
  titleKind: ConsentTitleKind;
  busy: boolean;
  onClose: () => void;
  onConfirm: (agentVisibleOperations: string[]) => Promise<void> | void;
}) {
  // Fresh installs keep every operation unchecked by default even when recommended.
  // Upgrades preserve existing retained selections while leaving newly added ones unchecked.
  const [selectedOps, setSelectedOps] = useState<Set<string>>(() =>
    preview.is_upgrade ? new Set(preview.existing_agent_visible_operations) : new Set(),
  );

  const newlyAdded = new Set(preview.newly_added_operations);
  const blocked = Boolean(preview.source_conflict);

  const toggleOperation = (opName: string) => {
    setSelectedOps((prev) => {
      const next = new Set(prev);
      if (next.has(opName)) {
        next.delete(opName);
      } else {
        next.add(opName);
      }
      return next;
    });
  };

  let title = m["mods.consentInstallTitle"]();
  let confirmLabel = m["mods.confirmInstall"]();
  if (titleKind === "reload") {
    title = m["mods.consentReloadTitle"]();
    confirmLabel = m["mods.confirmReload"]();
  } else if (titleKind === "upgrade" || preview.is_upgrade) {
    title = m["mods.consentUpgradeTitle"]();
    confirmLabel = m["mods.confirmUpgrade"]();
  }

  const handleConfirm = () => {
    const ordered = preview.operations.map((op) => op.name).filter((name) => selectedOps.has(name));
    void onConfirm(ordered);
  };

  return (
    <>
      <DialogHeader>
        <DialogTitle>{title}</DialogTitle>
        <DialogDescription>{m["mods.consentDescription"]()}</DialogDescription>
      </DialogHeader>

      <div className="flex max-h-[62vh] flex-col gap-3 overflow-y-auto pr-1">
        {/* Identity & Artifact Metadata */}
        <div className="rounded-lg border border-hair bg-ground p-3 text-caption">
          <div className="flex flex-wrap items-baseline justify-between gap-2">
            <span className="text-body-sm font-medium text-fg">{preview.name}</span>
            <span className="font-mono text-fg2">
              {preview.id} · v{preview.version} · API {preview.api}
            </span>
          </div>
          {(preview.author || preview.license) && (
            <div className="mt-1 text-fg2">
              {[preview.author, preview.license].filter(Boolean).join(" · ")}
            </div>
          )}
          <div className="mt-2 space-y-1 text-fg2">
            <div>
              <span className="font-medium text-fg">{m["mods.sourceLabel"]()}: </span>
              <span className="break-all font-mono">
                {formatSourceSummary(preview.source, sourcePath)}
              </span>
            </div>
            <div>
              <span className="font-medium text-fg">{m["mods.targetLabel"]()}: </span>
              <span className="font-mono">{preview.target}</span>
            </div>
            {mode === "archive" && preview.package_sha256 && (
              <div>
                <span className="font-medium text-fg">{m["mods.sha256Label"]()}: </span>
                <span data-testid="mod-consent-sha256" className="break-all font-mono">
                  {preview.package_sha256}
                </span>
              </div>
            )}
          </div>
        </div>

        {/* Process Boundary Warning */}
        <div className="flex items-start gap-2 rounded-lg border border-warn/40 bg-warn/10 p-3 text-caption text-fg">
          <ShieldAlert
            className="mt-0.5 size-4 shrink-0 text-warn"
            aria-hidden="true"
            strokeWidth={ICON_STROKE}
          />
          <span>{m["mods.processBoundaryWarning"]()}</span>
        </div>

        {/* Cross-Source Conflict Blocker */}
        {preview.source_conflict && (
          <div
            role="alert"
            data-testid="mod-consent-source-conflict"
            className="flex items-start gap-2 rounded-lg border border-err/40 bg-err/10 p-3 text-caption text-err"
          >
            <CircleAlert
              className="mt-0.5 size-4 shrink-0"
              aria-hidden="true"
              strokeWidth={ICON_STROKE}
            />
            <div>
              <div className="font-medium">{m["mods.sourceConflictTitle"]()}</div>
              <p className="mt-0.5">{preview.source_conflict}</p>
            </div>
          </div>
        )}

        {/* Permission Expansion Alert */}
        {preview.permission_expansions.length > 0 && (
          <div
            role="alert"
            data-testid="mod-consent-permission-expansions"
            className="rounded-lg border border-warn/50 bg-warn/10 p-3 text-caption"
          >
            <div className="font-medium text-fg">{m["mods.permissionExpansionTitle"]()}</div>
            <ul className="mt-1 list-disc space-y-0.5 pl-4 text-fg2">
              {preview.permission_expansions.map((reason) => (
                <li key={reason}>{reason}</li>
              ))}
            </ul>
          </div>
        )}

        {/* Declared Permissions */}
        <div className="rounded-lg border border-hair bg-ground p-3">
          <div className="mb-1.5 text-caption font-medium text-fg">
            {m["mods.permissionsTitle"]()}
          </div>
          <DeclaredPermissionsList permissions={preview.permissions} />
        </div>

        {/* Per-Operation Agent Visibility Selection */}
        <div className="rounded-lg border border-hair bg-ground p-3">
          <div className="text-caption font-medium text-fg">{m["mods.operationsTitle"]()}</div>
          <p className="mt-0.5 mb-2 text-caption text-fg2">{m["mods.operationsHelp"]()}</p>
          <div className="space-y-2">
            {preview.operations.map((op) => {
              const checked = selectedOps.has(op.name);
              const isNew = preview.is_upgrade && newlyAdded.has(op.name);
              return (
                <label
                  key={op.name}
                  className="flex cursor-pointer items-start gap-2.5 rounded-md border border-hair-soft bg-surface p-2.5"
                >
                  <input
                    type="checkbox"
                    checked={checked}
                    disabled={busy || blocked}
                    onChange={() => toggleOperation(op.name)}
                    aria-label={`${m["mods.exposeToAgent"]()}: ${op.name}`}
                    className="mt-0.5 size-4 rounded border-hair accent-accent"
                  />
                  <div className="min-w-0 flex-1">
                    <div className="flex flex-wrap items-center gap-1.5">
                      <span className="font-mono text-caption font-medium text-fg">{op.name}</span>
                      {op.recommended_for_agent && (
                        <span className="rounded bg-tile px-1.5 py-0.5 text-[11px] text-fg2">
                          {m["mods.authorRecommended"]()}
                        </span>
                      )}
                      {isNew && (
                        <span className="rounded bg-warn/15 px-1.5 py-0.5 text-[11px] font-medium text-warn">
                          {m["mods.newOperationBadge"]()}
                        </span>
                      )}
                      {op.read_only && (
                        <span className="rounded bg-tile px-1.5 py-0.5 text-[11px] text-fg2">
                          {m["mods.riskReadOnly"]()}
                        </span>
                      )}
                      {op.destructive && (
                        <span className="rounded bg-err/10 px-1.5 py-0.5 text-[11px] text-err">
                          {m["mods.riskDestructive"]()}
                        </span>
                      )}
                      {op.open_world && (
                        <span className="rounded bg-warn/10 px-1.5 py-0.5 text-[11px] text-warn">
                          {m["mods.riskOpenWorld"]()}
                        </span>
                      )}
                      {op.uses_host_path && (
                        <span className="rounded bg-warn/10 px-1.5 py-0.5 text-[11px] text-warn">
                          {m["mods.riskHostPath"]()}
                        </span>
                      )}
                    </div>
                    <p className="mt-0.5 text-caption text-fg2">{op.description}</p>
                  </div>
                </label>
              );
            })}
          </div>
        </div>
      </div>

      <DialogFooter>
        <Button type="button" variant="outline" disabled={busy} onClick={onClose}>
          {m["mods.cancel"]()}
        </Button>
        <Button type="button" disabled={busy || blocked} onClick={handleConfirm}>
          {busy ? m["mods.installing"]() : confirmLabel}
        </Button>
      </DialogFooter>
    </>
  );
}

/** Pre-install / upgrade / reload consent dialog for third-party and local mods. */
export function ModInstallConsentDialog({
  open,
  onOpenChange,
  mode,
  sourcePath,
  preview,
  titleKind = "install",
  busy,
  onConfirm,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  mode: "archive" | "directory";
  sourcePath: string;
  preview: ModInstallPreviewDto | null;
  titleKind?: ConsentTitleKind;
  busy: boolean;
  onConfirm: (agentVisibleOperations: string[]) => Promise<void> | void;
}) {
  if (!open || !preview) return null;

  const dialogKey = [
    preview.id,
    preview.version,
    preview.package_sha256 ?? sourcePath,
    preview.is_upgrade ? "1" : "0",
    titleKind,
  ].join(":");

  return (
    <Dialog open={open} onOpenChange={(next) => !busy && onOpenChange(next)}>
      <DialogContent
        showCloseButton={!busy}
        data-testid="mod-install-consent-dialog"
        className="sm:max-w-xl"
      >
        <ConsentDialogBody
          key={dialogKey}
          mode={mode}
          sourcePath={sourcePath}
          preview={preview}
          titleKind={titleKind}
          busy={busy}
          onClose={() => onOpenChange(false)}
          onConfirm={onConfirm}
        />
      </DialogContent>
    </Dialog>
  );
}
