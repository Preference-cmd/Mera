import { useState } from "react";
import {
  Folder,
  FolderOpen,
  PackagePlus,
  Plus,
  ShieldAlert,
  ShieldCheck,
  Trash2,
} from "lucide-react";
import * as m from "$paraglide/messages";
import { ICON_STROKE } from "@/design/tokens";
import type { ModRuntimeEnvironmentDto, ModSourcesListDto } from "@/ipc/schemas";
import { Button } from "@/components/ui/button";
import { SettingRow, SettingsCard, ValueChip } from "./SettingRow";

export function ModsSourcesTab({
  modsRoot,
  sources,
  runtimeStatus,
  uvBootstrapping,
  onOpenDir,
  onInstallArchive,
  onMountDirectory,
  onAddSource,
  onToggleSource,
  onRemoveSource,
  onBootstrapUv,
}: {
  modsRoot: string;
  sources: ModSourcesListDto | undefined;
  runtimeStatus: ModRuntimeEnvironmentDto | undefined;
  uvBootstrapping: boolean;
  onOpenDir: () => void;
  onInstallArchive: () => void;
  onMountDirectory: () => void;
  onAddSource: (input: {
    name: string;
    url: string;
    publicKeyEd25519?: string;
  }) => Promise<boolean>;
  onToggleSource: (sourceId: string, enabled: boolean) => void;
  onRemoveSource: (sourceId: string, name: string) => void;
  onBootstrapUv: () => void;
}) {
  const [name, setName] = useState("");
  const [url, setUrl] = useState("");
  const [publicKey, setPublicKey] = useState("");
  const [adding, setAdding] = useState(false);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!name.trim() || !url.trim()) return;
    setAdding(true);
    try {
      const ok = await onAddSource({
        name: name.trim(),
        url: url.trim(),
        publicKeyEd25519: publicKey.trim() || undefined,
      });
      if (ok) {
        setName("");
        setUrl("");
        setPublicKey("");
      }
    } finally {
      setAdding(false);
    }
  };

  return (
    <div className="space-y-4">
      {/* Official Registry */}
      <SettingsCard>
        <SettingRow
          labelId="mods-official-source"
          label={m["mods.sourcesOfficialTitle"]()}
          description={`${m["mods.sourcesOfficialDescription"]()} (${sources?.official_url ?? "…"})`}
        >
          <ValueChip>
            <span className="inline-flex items-center gap-1">
              <ShieldCheck
                className="size-3.5 text-accent"
                aria-hidden="true"
                strokeWidth={ICON_STROKE}
              />
              TUF Ed25519
            </span>
          </ValueChip>
        </SettingRow>
      </SettingsCard>

      {/* Third-Party Registries */}
      <SettingsCard>
        <div className="space-y-3 px-5 py-4">
          <div className="text-body-sm font-medium text-fg">
            {m["mods.sourcesThirdPartyTitle"]()}
          </div>
          <div className="flex items-start gap-2 rounded-lg border border-warn/40 bg-warn/10 p-2.5 text-caption text-fg">
            <ShieldAlert
              className="mt-0.5 size-4 shrink-0 text-warn"
              aria-hidden="true"
              strokeWidth={ICON_STROKE}
            />
            <span>{m["mods.sourcesThirdPartyWarning"]()}</span>
          </div>

          <form
            onSubmit={(e) => void handleSubmit(e)}
            className="space-y-2 rounded-lg border border-hair-soft bg-ground p-3"
          >
            <div className="text-caption font-medium text-fg">{m["mods.sourcesAddTitle"]()}</div>
            <div className="grid gap-2 sm:grid-cols-2">
              <input
                type="text"
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder={m["mods.sourcesNamePlaceholder"]()}
                aria-label={m["mods.sourcesNameLabel"]()}
                required
                className="h-8 rounded-md border border-hair bg-surface px-2.5 text-caption text-fg placeholder:text-fg2 focus:border-accent focus:outline-none"
              />
              <input
                type="url"
                value={url}
                onChange={(e) => setUrl(e.target.value)}
                placeholder={m["mods.sourcesUrlPlaceholder"]()}
                aria-label={m["mods.sourcesUrlLabel"]()}
                required
                className="h-8 rounded-md border border-hair bg-surface px-2.5 text-caption text-fg placeholder:text-fg2 focus:border-accent focus:outline-none"
              />
            </div>
            <div className="flex flex-wrap items-center gap-2">
              <input
                type="text"
                value={publicKey}
                onChange={(e) => setPublicKey(e.target.value)}
                placeholder={m["mods.sourcesKeyPlaceholder"]()}
                aria-label={m["mods.sourcesKeyLabel"]()}
                className="h-8 min-w-[220px] flex-1 rounded-md border border-hair bg-surface px-2.5 font-mono text-caption text-fg placeholder:text-fg2 focus:border-accent focus:outline-none"
              />
              <Button
                type="submit"
                size="sm"
                disabled={adding}
                className="h-8 gap-1.5 text-caption"
              >
                <Plus className="size-3.5" aria-hidden="true" strokeWidth={ICON_STROKE} />
                {m["mods.sourcesAddButton"]()}
              </Button>
            </div>
          </form>

          {(sources?.third_party ?? []).length === 0 ? (
            <p className="text-caption text-fg2">{m["mods.sourcesEmpty"]()}</p>
          ) : (
            <div className="space-y-2">
              {sources?.third_party.map((src) => (
                <div
                  key={src.id}
                  data-testid={`third-party-source-${src.id}`}
                  className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-hair-soft bg-ground px-3.5 py-2.5 text-caption"
                >
                  <div className="min-w-0 flex-1">
                    <div className="flex flex-wrap items-center gap-2">
                      <span className="font-medium text-fg">{src.name}</span>
                      {src.has_public_key && (
                        <span className="rounded bg-tile px-1.5 py-0.5 text-[11px] text-fg2">
                          Ed25519
                        </span>
                      )}
                    </div>
                    <div className="break-all font-mono text-[11px] text-fg2">{src.url}</div>
                    {src.last_error && (
                      <div className="mt-0.5 text-[11px] text-err">{src.last_error}</div>
                    )}
                  </div>
                  <div className="flex items-center gap-2">
                    <label className="flex cursor-pointer items-center gap-1.5 text-[11px] text-fg2">
                      <input
                        type="checkbox"
                        checked={src.enabled}
                        onChange={(e) => onToggleSource(src.id, e.target.checked)}
                        className="size-3.5 accent-accent"
                      />
                      <span>
                        {src.enabled ? m["mods.status.available"]() : m["mods.status.disabled"]()}
                      </span>
                    </label>
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      onClick={() => onRemoveSource(src.id, src.name)}
                      className="h-7 gap-1 px-2 text-caption text-fg2 hover:text-err"
                    >
                      <Trash2 className="size-3" aria-hidden="true" strokeWidth={ICON_STROKE} />
                      {m["mods.sourcesRemove"]()}
                    </Button>
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>
      </SettingsCard>

      {/* Local Packages & Dev Folder */}
      <SettingsCard>
        <SettingRow
          labelId="mods-root-label"
          label={m["mods.directoryLabel"]()}
          description={modsRoot}
        >
          <Button type="button" variant="outline" size="sm" onClick={onOpenDir} className="gap-1.5">
            <FolderOpen className="size-3.5" aria-hidden="true" strokeWidth={ICON_STROKE} />
            {m["mods.openDir"]()}
          </Button>
        </SettingRow>
        <div className="flex flex-wrap items-center gap-2 px-5 py-3">
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={onInstallArchive}
            className="gap-1.5"
          >
            <PackagePlus className="size-3.5" aria-hidden="true" strokeWidth={ICON_STROKE} />
            {m["mods.installArchive"]()}
          </Button>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={onMountDirectory}
            className="gap-1.5"
          >
            <Folder className="size-3.5" aria-hidden="true" strokeWidth={ICON_STROKE} />
            {m["mods.mountDirectory"]()}
          </Button>
        </div>
      </SettingsCard>

      {/* Sandbox & Optional uv Runtime */}
      {runtimeStatus && (
        <SettingsCard>
          <SettingRow
            labelId="mods-uv-runtime"
            label={m["mods.uvRuntimeTitle"]()}
            description={
              runtimeStatus.uv.available
                ? `${m["mods.uvAvailable"]({ source: runtimeStatus.uv.source })} — ${runtimeStatus.uv.path ?? ""}`
                : m["mods.uvMissing"]()
            }
          >
            {runtimeStatus.uv.available ? (
              <ValueChip>
                {runtimeStatus.sandbox_enforced
                  ? m["mods.sandboxEnforced"]({ backend: runtimeStatus.sandbox_backend })
                  : m["mods.sandboxFallback"]()}
              </ValueChip>
            ) : (
              <Button
                type="button"
                variant="outline"
                size="sm"
                disabled={!runtimeStatus.uv_bootstrap_supported || uvBootstrapping}
                onClick={onBootstrapUv}
                className="text-caption"
              >
                {uvBootstrapping ? m["mods.uvInstalling"]() : m["mods.uvInstallPortable"]()}
              </Button>
            )}
          </SettingRow>
        </SettingsCard>
      )}
    </div>
  );
}
