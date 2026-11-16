import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ChevronDown,
  ChevronRight,
  CircleAlert,
  Folder,
  FolderOpen,
  PackagePlus,
  RefreshCw,
  Trash2,
} from "lucide-react";
import { toast } from "sonner";
import * as m from "$paraglide/messages";
import { ICON_STROKE } from "@/design/tokens";
import { cn } from "@/lib/utils";
import {
  isTauri,
  modsInstallArchive,
  modsInstallDirectory,
  modsList,
  modsOpenDir,
  modsPreviewArchive,
  modsPreviewDirectory,
  modsReloadLocal,
  modsSetAgentVisibility,
  modsSetEnabled,
  modsUninstall,
  subscribeEvents,
  toCommandError,
} from "@/ipc";
import type { ModEntryDto, ModInstallPreviewDto, ModSourceDto } from "@/ipc/schemas";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  DeclaredPermissionsList,
  ModInstallConsentDialog,
  type ConsentTitleKind,
} from "./ModInstallConsentDialog";
import { SettingRow, SettingsCard, ValueChip } from "./SettingRow";

function sourceBadgeLabel(source: ModSourceDto): string {
  switch (source.kind) {
    case "bundled":
      return m["mods.source.bundled"]();
    case "official":
      return m["mods.source.official"]();
    case "third_party_url":
      return m["mods.source.thirdPartyUrl"]();
    case "local_archive":
      return m["mods.source.localArchive"]();
    case "local_directory":
      return m["mods.source.localDirectory"]();
  }
}

function sourceDetailLine(source: ModSourceDto): string | null {
  switch (source.kind) {
    case "bundled":
      return null;
    case "official":
      return source.catalog_url;
    case "third_party_url":
      return source.source_url;
    case "local_archive":
      return source.filename;
    case "local_directory":
      return source.path;
  }
}

function formatModError(err: unknown): string {
  const structured = toCommandError(err);
  if (structured) return structured.message;
  if (err instanceof Error && err.message) return err.message;
  return m["mods.actionFailed"]();
}

function ModSwitch({
  checked,
  disabled,
  label,
  onToggle,
}: {
  checked: boolean;
  disabled?: boolean;
  label: string;
  onToggle?: () => void;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      disabled={disabled}
      onClick={onToggle}
      className={cn(
        "relative inline-flex h-5 w-9 shrink-0 items-center rounded-full border transition-colors",
        checked ? "border-accent bg-accent" : "border-hair bg-app",
        disabled ? "cursor-not-allowed opacity-60" : "cursor-pointer",
      )}
    >
      <span
        className={cn(
          "inline-block size-3.5 rounded-full bg-surface shadow-xs transition-transform",
          checked ? "translate-x-4" : "translate-x-0.5",
        )}
      />
    </button>
  );
}

type PendingConsentState = {
  mode: "archive" | "directory";
  sourcePath: string;
  preview: ModInstallPreviewDto;
  titleKind: ConsentTitleKind;
};

/** Settings -> App -> Mods management section (M2 single compact view). */
export function ModsSettingsSection() {
  const queryClient = useQueryClient();
  const [busyModIds, setBusyModIds] = useState<Set<string>>(() => new Set());
  const [installBusy, setInstallBusy] = useState(false);
  const [collapsedIds, setCollapsedIds] = useState<Set<string>>(() => new Set());
  const [consentState, setConsentState] = useState<PendingConsentState | null>(null);
  const [uninstallTarget, setUninstallTarget] = useState<ModEntryDto | null>(null);
  const [removeFiles, setRemoveFiles] = useState(true);
  const [uninstallBusy, setUninstallBusy] = useState(false);

  const { data, isLoading, error } = useQuery({
    queryKey: ["mods"],
    queryFn: modsList,
    staleTime: 0,
  });

  useEffect(() => {
    return subscribeEvents((event) => {
      if (event.type === "mod" && event.kind.startsWith("mod.")) {
        void queryClient.invalidateQueries({ queryKey: ["mods"] });
      }
    });
  }, [queryClient]);

  const setModBusy = (modId: string, busy: boolean) => {
    setBusyModIds((prev) => {
      const next = new Set(prev);
      if (busy) {
        next.add(modId);
      } else {
        next.delete(modId);
      }
      return next;
    });
  };

  const toggleDetails = (modId: string) => {
    setCollapsedIds((prev) => {
      const next = new Set(prev);
      if (next.has(modId)) {
        next.delete(modId);
      } else {
        next.add(modId);
      }
      return next;
    });
  };

  const handleOpenDir = async () => {
    try {
      await modsOpenDir();
    } catch (err) {
      toast.error(formatModError(err));
    }
  };

  const openArchivePreview = async (path: string) => {
    try {
      const preview = await modsPreviewArchive(path);
      setConsentState({
        mode: "archive",
        sourcePath: path,
        preview,
        titleKind: preview.is_upgrade ? "upgrade" : "install",
      });
    } catch (err) {
      toast.error(formatModError(err));
    }
  };

  const openDirectoryPreview = async (path: string, titleKindOverride?: ConsentTitleKind) => {
    try {
      const preview = await modsPreviewDirectory(path);
      setConsentState({
        mode: "directory",
        sourcePath: path,
        preview,
        titleKind: titleKindOverride ?? (preview.is_upgrade ? "upgrade" : "install"),
      });
    } catch (err) {
      toast.error(formatModError(err));
    }
  };

  const handleInstallFromPackage = async () => {
    if (!isTauri()) {
      await openArchivePreview("/mock/markdown_tools-0.1.0.rmod.tar.gz");
      return;
    }
    try {
      const { open: openDialog } = await import("@tauri-apps/plugin-dialog");
      const selected = await openDialog({
        multiple: false,
        filters: [{ name: "Mera Mod Package", extensions: ["gz"] }],
      });
      if (!selected || typeof selected !== "string") return;
      await openArchivePreview(selected);
    } catch (err) {
      toast.error(formatModError(err));
    }
  };

  const handleMountDevFolder = async () => {
    if (!isTauri()) {
      await openDirectoryPreview("/mock/dev/markdown_tools");
      return;
    }
    try {
      const { open: openDialog } = await import("@tauri-apps/plugin-dialog");
      const selected = await openDialog({ directory: true, multiple: false });
      if (!selected || typeof selected !== "string") return;
      await openDirectoryPreview(selected);
    } catch (err) {
      toast.error(formatModError(err));
    }
  };

  const handleConfirmConsent = async (agentVisibleOperations: string[]) => {
    if (!consentState) return;
    const { mode, sourcePath, preview, titleKind } = consentState;
    setInstallBusy(true);
    try {
      if (titleKind === "reload") {
        await modsReloadLocal(preview.id, true);
        const existingSet = new Set(preview.existing_agent_visible_operations);
        const changed =
          agentVisibleOperations.length !== existingSet.size ||
          agentVisibleOperations.some((op) => !existingSet.has(op));
        if (changed) {
          await modsSetAgentVisibility(preview.id, agentVisibleOperations);
        }
        toast.success(m["mods.reloadedToast"]({ name: preview.name }));
      } else if (mode === "archive") {
        await modsInstallArchive({
          path: sourcePath,
          consentGranted: true,
          consentPermissionExpansion: true,
          enabled: true,
          agentVisibleOperations,
        });
        toast.success(
          preview.is_upgrade
            ? m["mods.upgradedToast"]({ name: preview.name, version: preview.version })
            : m["mods.installedToast"]({ name: preview.name, version: preview.version }),
        );
      } else {
        await modsInstallDirectory({
          path: sourcePath,
          consentGranted: true,
          consentPermissionExpansion: true,
          enabled: true,
          agentVisibleOperations,
        });
        toast.success(
          preview.is_upgrade
            ? m["mods.upgradedToast"]({ name: preview.name, version: preview.version })
            : m["mods.installedToast"]({ name: preview.name, version: preview.version }),
        );
      }
      setConsentState(null);
      await queryClient.invalidateQueries({ queryKey: ["mods"] });
    } catch (err) {
      toast.error(formatModError(err));
    } finally {
      setInstallBusy(false);
    }
  };

  const handleToggleEnabled = async (mod: ModEntryDto) => {
    if (mod.read_only_bundled) return;
    setModBusy(mod.id, true);
    try {
      await modsSetEnabled(mod.id, !mod.enabled);
      await queryClient.invalidateQueries({ queryKey: ["mods"] });
    } catch (err) {
      toast.error(formatModError(err));
    } finally {
      setModBusy(mod.id, false);
    }
  };

  const handleToggleOperationVisibility = async (mod: ModEntryDto, opName: string) => {
    if (mod.read_only_bundled) return;
    setModBusy(mod.id, true);
    try {
      const nextSet = new Set(mod.agent_visible_operations);
      if (nextSet.has(opName)) {
        nextSet.delete(opName);
      } else {
        nextSet.add(opName);
      }
      const nextOps = mod.operations.map((op) => op.name).filter((name) => nextSet.has(name));
      await modsSetAgentVisibility(mod.id, nextOps);
      await queryClient.invalidateQueries({ queryKey: ["mods"] });
    } catch (err) {
      toast.error(formatModError(err));
    } finally {
      setModBusy(mod.id, false);
    }
  };

  const handleReloadLocal = async (mod: ModEntryDto) => {
    setModBusy(mod.id, true);
    try {
      const updated = await modsReloadLocal(mod.id, false);
      toast.success(m["mods.reloadedToast"]({ name: updated.name }));
      await queryClient.invalidateQueries({ queryKey: ["mods"] });
    } catch (err) {
      const structured = toCommandError(err);
      if (
        structured?.code === "permission_denied" &&
        structured.details?.kind === "permission_expansion" &&
        mod.source.kind === "local_directory"
      ) {
        await openDirectoryPreview(mod.source.path, "reload");
      } else {
        toast.error(formatModError(err));
      }
    } finally {
      setModBusy(mod.id, false);
    }
  };

  const openUninstallDialog = (mod: ModEntryDto) => {
    setUninstallTarget(mod);
    setRemoveFiles(mod.source.kind !== "local_directory");
  };

  const handleConfirmUninstall = async () => {
    if (!uninstallTarget) return;
    setUninstallBusy(true);
    const target = uninstallTarget;
    try {
      const shouldRemoveFiles = target.source.kind === "local_directory" ? false : removeFiles;
      await modsUninstall(target.id, shouldRemoveFiles);
      setUninstallTarget(null);
      toast.success(m["mods.uninstalledToast"]({ name: target.name }));
      await queryClient.invalidateQueries({ queryKey: ["mods"] });
    } catch (err) {
      toast.error(formatModError(err));
    } finally {
      setUninstallBusy(false);
    }
  };

  const mods = data?.mods ?? [];

  return (
    <div className="space-y-4">
      <p className="text-caption text-fg2">{m["mods.subtitle"]()}</p>

      {/* Mods Directory & Install Entry Points */}
      <SettingsCard>
        <SettingRow
          labelId="mods-root-label"
          label={m["mods.directoryLabel"]()}
          description={data?.mods_root ?? "…"}
        >
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => void handleOpenDir()}
            className="gap-1.5"
          >
            <FolderOpen className="size-3.5" aria-hidden="true" strokeWidth={ICON_STROKE} />
            {m["mods.openDir"]()}
          </Button>
        </SettingRow>
        <div className="flex flex-wrap items-center gap-2 px-5 py-3">
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => void handleInstallFromPackage()}
            className="gap-1.5"
          >
            <PackagePlus className="size-3.5" aria-hidden="true" strokeWidth={ICON_STROKE} />
            {m["mods.installArchive"]()}
          </Button>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => void handleMountDevFolder()}
            className="gap-1.5"
          >
            <Folder className="size-3.5" aria-hidden="true" strokeWidth={ICON_STROKE} />
            {m["mods.mountDirectory"]()}
          </Button>
        </div>
      </SettingsCard>

      {/* Installed Mods List */}
      {isLoading ? (
        <div className="rounded-xl border border-hair bg-surface p-5 text-caption text-fg2">
          {m["mods.loading"]()}
        </div>
      ) : error ? (
        <div
          role="alert"
          className="rounded-xl border border-err/40 bg-err/10 p-4 text-caption text-err"
        >
          {formatModError(error)}
        </div>
      ) : mods.length === 0 ? (
        <div className="rounded-xl border border-hair bg-surface p-5 text-caption text-fg2">
          {m["mods.empty"]()}
        </div>
      ) : (
        <div className="space-y-3">
          {mods.map((mod) => {
            const busy = busyModIds.has(mod.id);
            const detailsOpen = !collapsedIds.has(mod.id);
            const isUnavailable = mod.health.status === "unavailable";
            const visibleCount = mod.agent_visible_operations.length;
            const totalCount = mod.operations.length;
            const srcDetail = sourceDetailLine(mod.source);

            let statusBadge = m["mods.status.available"]();
            let statusClass = "bg-tile text-fg2";
            if (mod.read_only_bundled) {
              statusBadge = m["mods.status.alwaysActive"]();
            } else if (!mod.enabled) {
              statusBadge = m["mods.status.disabled"]();
            } else if (isUnavailable) {
              statusBadge = m["mods.status.unavailable"]();
              statusClass = "bg-err/10 text-err";
            }

            return (
              <div
                key={mod.id}
                data-testid={`mod-card-${mod.id}`}
                className="overflow-hidden rounded-xl border border-hair bg-surface"
              >
                <div className="flex flex-wrap items-center justify-between gap-3 px-5 py-3.5">
                  <div className="min-w-0 flex-1">
                    <div className="flex flex-wrap items-center gap-2">
                      <span className="text-body-sm font-medium text-fg">{mod.name}</span>
                      <span className="font-mono text-caption text-fg2">
                        {mod.id} · v{mod.version}
                      </span>
                      <span className="rounded bg-tile px-1.5 py-0.5 text-[11px] font-medium text-fg2">
                        {sourceBadgeLabel(mod.source)}
                      </span>
                      <span
                        className={cn("rounded px-1.5 py-0.5 text-[11px] font-medium", statusClass)}
                      >
                        {statusBadge}
                      </span>
                      <span
                        data-testid={`mod-agent-count-${mod.id}`}
                        className="rounded border border-agent-line bg-agent-soft px-1.5 py-0.5 font-mono text-[11px] text-agent"
                      >
                        {m["mods.agentCount"]({ visible: visibleCount, total: totalCount })}
                      </span>
                    </div>
                  </div>

                  <div className="flex shrink-0 items-center gap-2">
                    {mod.read_only_bundled ? (
                      <>
                        <ValueChip>{m["mods.status.alwaysActive"]()}</ValueChip>
                        <ModSwitch
                          checked
                          disabled
                          label={m["mods.enableAria"]({ name: mod.name })}
                        />
                      </>
                    ) : (
                      <>
                        {mod.source.kind === "local_directory" && (
                          <Button
                            type="button"
                            variant="outline"
                            size="sm"
                            disabled={busy}
                            onClick={() => void handleReloadLocal(mod)}
                            className="h-7 gap-1 px-2 text-caption"
                          >
                            <RefreshCw
                              className="size-3"
                              aria-hidden="true"
                              strokeWidth={ICON_STROKE}
                            />
                            {m["mods.reload"]()}
                          </Button>
                        )}
                        <Button
                          type="button"
                          variant="ghost"
                          size="sm"
                          disabled={busy}
                          onClick={() => openUninstallDialog(mod)}
                          className="h-7 gap-1 px-2 text-caption text-fg2 hover:text-err"
                        >
                          <Trash2 className="size-3" aria-hidden="true" strokeWidth={ICON_STROKE} />
                          {m["mods.uninstall"]()}
                        </Button>
                        <ModSwitch
                          checked={mod.enabled}
                          disabled={busy}
                          label={m["mods.enableAria"]({ name: mod.name })}
                          onToggle={() => void handleToggleEnabled(mod)}
                        />
                      </>
                    )}
                  </div>
                </div>

                {/* Unhealthy diagnostic alert when enabled or reporting diagnostic */}
                {mod.enabled && isUnavailable && mod.health.message && (
                  <div
                    role="alert"
                    data-testid={`mod-diagnostic-${mod.id}`}
                    className="mx-5 mb-3 flex items-start gap-2 rounded-lg border border-err/40 bg-err/10 p-2.5 text-caption text-err"
                  >
                    <CircleAlert
                      className="mt-0.5 size-4 shrink-0"
                      aria-hidden="true"
                      strokeWidth={ICON_STROKE}
                    />
                    <span>
                      {mod.health.diagnostic_code ? `${mod.health.diagnostic_code}: ` : ""}
                      {mod.health.message}
                    </span>
                  </div>
                )}

                <div className="border-t border-hair-soft px-5 py-2">
                  <button
                    type="button"
                    aria-expanded={detailsOpen}
                    onClick={() => toggleDetails(mod.id)}
                    className="flex items-center gap-1 text-caption font-medium text-fg2 hover:text-fg"
                  >
                    {detailsOpen ? (
                      <ChevronDown
                        className="size-3.5"
                        aria-hidden="true"
                        strokeWidth={ICON_STROKE}
                      />
                    ) : (
                      <ChevronRight
                        className="size-3.5"
                        aria-hidden="true"
                        strokeWidth={ICON_STROKE}
                      />
                    )}
                    {detailsOpen ? m["mods.hideDetails"]() : m["mods.details"]()}
                  </button>

                  {detailsOpen && (
                    <div
                      data-testid={`mod-details-${mod.id}`}
                      className="mt-2.5 space-y-3 pb-2 text-caption"
                    >
                      {(srcDetail || mod.package_sha256) && (
                        <div className="space-y-1 text-fg2">
                          {srcDetail && (
                            <div className="break-all">
                              <span className="font-medium text-fg">
                                {m["mods.installLocation"]()}:{" "}
                              </span>
                              <span className="font-mono">{srcDetail}</span>
                            </div>
                          )}
                          {mod.package_sha256 && (
                            <div className="break-all">
                              <span className="font-medium text-fg">
                                {m["mods.sha256Label"]()}:{" "}
                              </span>
                              <span className="font-mono">{mod.package_sha256}</span>
                            </div>
                          )}
                        </div>
                      )}

                      <div>
                        <div className="mb-1 font-medium text-fg">
                          {m["mods.permissionsTitle"]()}
                        </div>
                        <DeclaredPermissionsList permissions={mod.permissions} />
                      </div>

                      <div>
                        <div className="mb-1 flex flex-wrap items-center justify-between gap-2">
                          <span className="font-medium text-fg">{m["mods.operationsTitle"]()}</span>
                        </div>
                        <p className="mb-2 text-fg2">
                          {mod.read_only_bundled
                            ? m["mods.bundledReadOnlyNote"]()
                            : m["mods.operationsHelp"]()}
                        </p>
                        <div className="space-y-1.5">
                          {mod.operations.map((op) => {
                            const visible = mod.agent_visible_operations.includes(op.name);
                            const opDisabled = mod.read_only_bundled || busy;
                            return (
                              <label
                                key={op.name}
                                data-testid={`mod-op-${op.name}`}
                                className={cn(
                                  "flex items-start gap-2.5 rounded-md border border-hair-soft bg-ground p-2.5",
                                  opDisabled ? "opacity-80" : "cursor-pointer",
                                )}
                              >
                                <input
                                  type="checkbox"
                                  checked={visible}
                                  disabled={opDisabled}
                                  onChange={() =>
                                    void handleToggleOperationVisibility(mod, op.name)
                                  }
                                  aria-label={`${m["mods.exposeToAgent"]()}: ${op.name}`}
                                  className="mt-0.5 size-4 rounded border-hair accent-accent"
                                />
                                <div className="min-w-0 flex-1">
                                  <div className="flex flex-wrap items-center gap-1.5">
                                    <span className="font-mono font-medium text-fg">{op.name}</span>
                                    {!mod.read_only_bundled && op.recommended_for_agent && (
                                      <span className="rounded bg-tile px-1.5 py-0.5 text-[11px] text-fg2">
                                        {m["mods.authorRecommended"]()}
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
                                  <p className="mt-0.5 text-fg2">{op.description}</p>
                                </div>
                              </label>
                            );
                          })}
                        </div>
                      </div>
                    </div>
                  )}
                </div>
              </div>
            );
          })}
        </div>
      )}

      {/* Pre-install / upgrade / reload consent dialog */}
      <ModInstallConsentDialog
        open={consentState !== null}
        onOpenChange={(next) => !next && setConsentState(null)}
        mode={consentState?.mode ?? "archive"}
        sourcePath={consentState?.sourcePath ?? ""}
        preview={consentState?.preview ?? null}
        titleKind={consentState?.titleKind ?? "install"}
        busy={installBusy}
        onConfirm={handleConfirmConsent}
      />

      {/* Uninstall confirmation dialog */}
      <Dialog
        open={uninstallTarget !== null}
        onOpenChange={(next) => !next && !uninstallBusy && setUninstallTarget(null)}
      >
        <DialogContent
          showCloseButton={!uninstallBusy}
          data-testid="mod-uninstall-dialog"
          className="sm:max-w-md"
        >
          <DialogHeader>
            <DialogTitle>
              {uninstallTarget
                ? m["mods.uninstallConfirmTitle"]({ name: uninstallTarget.name })
                : ""}
            </DialogTitle>
            <DialogDescription>{m["mods.uninstallConfirmDescription"]()}</DialogDescription>
          </DialogHeader>

          {uninstallTarget?.source.kind === "local_directory" ? (
            <p className="text-caption text-fg2">{m["mods.devFilesPreservedNote"]()}</p>
          ) : (
            <label className="flex cursor-pointer items-center gap-2 text-caption text-fg">
              <input
                type="checkbox"
                checked={removeFiles}
                disabled={uninstallBusy}
                onChange={(e) => setRemoveFiles(e.target.checked)}
                className="size-4 rounded border-hair accent-accent"
              />
              <span>{m["mods.removeFilesLabel"]()}</span>
            </label>
          )}

          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              disabled={uninstallBusy}
              onClick={() => setUninstallTarget(null)}
            >
              {m["mods.cancel"]()}
            </Button>
            <Button
              type="button"
              variant="destructive"
              disabled={uninstallBusy}
              onClick={() => void handleConfirmUninstall()}
            >
              {m["mods.confirmUninstall"]()}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
