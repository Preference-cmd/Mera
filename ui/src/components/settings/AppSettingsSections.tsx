import { useEffect, useState } from "react";
import { ChevronDown } from "lucide-react";
import * as m from "$paraglide/messages";
import { getLocale, setLocale } from "$paraglide/runtime";
import { version as packageVersion } from "../../../package.json";
import { cn } from "@/lib/utils";
import { ICON_STROKE } from "@/design/tokens";
import { useTheme, type ThemeMode } from "@/lib/theme";
import { isTauri } from "@/ipc";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { applyDocumentLang, isMacPlatform } from "@/components/layout/shellChrome";
import { SettingRow, SettingsCard, ValueChip } from "./SettingRow";

export function GeneralSection() {
  const locale = getLocale() === "zh" ? "zh" : "en";
  const label = locale === "zh" ? m["shell.languageChinese"]() : m["shell.languageEnglish"]();
  return (
    <SettingsCard>
      <SettingRow labelId="setting-language-label" label={m["settings.language"]()}>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <button
              type="button"
              aria-labelledby="setting-language-label setting-language-value"
              className="flex h-8 items-center gap-1.5 rounded-md border border-hair bg-surface px-2.5 text-[13px] font-medium text-fg transition-colors hover:bg-hover focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
            >
              <span id="setting-language-value">{label}</span>
              <ChevronDown className="size-3.5 text-fg3" strokeWidth={ICON_STROKE} />
            </button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="min-w-[140px]">
            <DropdownMenuRadioGroup
              value={locale}
              onValueChange={(value) => {
                const next = value as "en" | "zh";
                setLocale(next);
                applyDocumentLang(next);
              }}
            >
              <DropdownMenuRadioItem value="en">
                {m["shell.languageEnglish"]()}
              </DropdownMenuRadioItem>
              <DropdownMenuRadioItem value="zh">
                {m["shell.languageChinese"]()}
              </DropdownMenuRadioItem>
            </DropdownMenuRadioGroup>
          </DropdownMenuContent>
        </DropdownMenu>
      </SettingRow>
    </SettingsCard>
  );
}

export function AppearanceSection() {
  const { themeMode, onThemeModeChange } = useTheme();
  const options: { value: ThemeMode; label: string }[] = [
    { value: "light", label: m["settings.light"]() },
    { value: "dark", label: m["settings.dark"]() },
  ];
  return (
    <SettingsCard>
      <SettingRow
        labelId="setting-color-mode-label"
        label={m["settings.color-mode"]()}
        description={m["settings.color-mode-desc"]()}
      >
        <div
          role="radiogroup"
          aria-labelledby="setting-color-mode-label"
          className="flex rounded-md border border-hair-soft bg-app p-0.5"
        >
          {options.map((option) => {
            const checked = themeMode === option.value;
            return (
              <button
                key={option.value}
                type="button"
                role="radio"
                aria-checked={checked}
                onClick={() => onThemeModeChange(option.value)}
                className={cn(
                  "flex-1 rounded px-3 py-1 text-[13px] leading-[18px] font-medium transition-all focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent",
                  checked
                    ? "bg-surface text-fg shadow-[var(--shadow-1)]"
                    : "text-fg2 hover:bg-hover hover:text-fg",
                )}
              >
                {option.label}
              </button>
            );
          })}
        </div>
      </SettingRow>
    </SettingsCard>
  );
}

type Shortcut = { id: string; label: string; description?: string; keys: string };

/** The key bindings the app registers, spelled for the current platform. */
function shortcutList(mac: boolean): Shortcut[] {
  const mod = (key: string, shift = false) =>
    mac ? `${shift ? "⇧" : ""}⌘${key}` : `Ctrl+${shift ? "Shift+" : ""}${key}`;
  return [
    { id: "palette", label: m["settings.shortcutPalette"](), keys: mod("P") },
    { id: "settings", label: m["settings.shortcutSettings"](), keys: mod(",") },
    {
      id: "undo",
      label: m["settings.undo"](),
      description: m["settings.shortcutUndoDesc"](),
      keys: mod("Z"),
    },
    {
      id: "redo",
      label: m["settings.redo"](),
      description: m["settings.shortcutUndoDesc"](),
      keys: mod("Z", true),
    },
    {
      id: "delete",
      label: m["settings.shortcutDelete"](),
      description: m["settings.shortcutDeleteDesc"](),
      keys: mac ? "⌫" : "Delete",
    },
    {
      id: "finish-note",
      label: m["settings.shortcutFinishNote"](),
      keys: mac ? "⌘↵" : "Ctrl+Enter",
    },
    { id: "escape", label: m["settings.shortcutEscape"](), keys: "Esc" },
  ];
}

export function ShortcutsSection() {
  const shortcuts = shortcutList(isMacPlatform());
  return (
    <SettingsCard>
      {shortcuts.map((shortcut) => (
        <SettingRow
          key={shortcut.id}
          labelId={`setting-shortcut-${shortcut.id}-label`}
          label={shortcut.label}
          description={shortcut.description}
        >
          <kbd
            aria-labelledby={`setting-shortcut-${shortcut.id}-label`}
            className="inline-flex h-7 min-w-8 items-center justify-center rounded-md border border-hair bg-app px-2 font-sans text-[13px] font-medium text-fg2"
          >
            {shortcut.keys}
          </kbd>
        </SettingRow>
      ))}
    </SettingsCard>
  );
}

/** The running app's version: Tauri's when available, else the UI package's. */
function useAppVersion(): string {
  const [version, setVersion] = useState<string>(packageVersion);
  useEffect(() => {
    if (!isTauri()) return;
    let cancelled = false;
    void import("@tauri-apps/api/app")
      .then(({ getVersion }) => getVersion())
      .then((v) => {
        if (!cancelled) setVersion(v);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, []);
  return version;
}

export function AboutSection() {
  const version = useAppVersion();
  return (
    <SettingsCard>
      <SettingRow labelId="setting-version-label" label={m["settings.version"]()}>
        <ValueChip>{version}</ValueChip>
      </SettingRow>
      <SettingRow
        labelId="setting-backend-label"
        label={m["settings.inferenceBackend"]()}
        description={m["settings.inferenceBackendDesc"]()}
      >
        <ValueChip>{m["settings.burn"]()}</ValueChip>
      </SettingRow>
    </SettingsCard>
  );
}
