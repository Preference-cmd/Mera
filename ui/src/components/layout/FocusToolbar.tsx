import { useEffect, useRef, useState } from "react";
import { Check, CircleAlert, Plus, Type } from "lucide-react";
import * as m from "$paraglide/messages";
import { ICON_STROKE } from "@/design/tokens";
import { Button } from "@/components/ui/button";
import type { OperationInfo } from "@/ipc/schemas";
import {
  readPinnedTextTools,
  textOperationLabel,
  writePinnedTextTools,
} from "@/lib/textOperations";

export function FocusToolbar({
  slotId,
  availableOperations,
  loadError,
  onRetry,
  onSelectOperation,
}: {
  slotId: string;
  availableOperations: OperationInfo[];
  loadError: string | null;
  onRetry: () => void;
  onSelectOperation: (operation: OperationInfo) => void;
}) {
  const [pinnedNames, setPinnedNames] = useState<string[]>(() => readPinnedTextTools());
  const [menuOpen, setMenuOpen] = useState(false);
  const menuRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!menuOpen) return;
    function handleOutsideClick(event: MouseEvent) {
      if (menuRef.current && !menuRef.current.contains(event.target as Node)) {
        setMenuOpen(false);
      }
    }
    function handleEscape(event: KeyboardEvent) {
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        setMenuOpen(false);
      }
    }
    document.addEventListener("mousedown", handleOutsideClick);
    window.addEventListener("keydown", handleEscape, { capture: true });
    return () => {
      document.removeEventListener("mousedown", handleOutsideClick);
      window.removeEventListener("keydown", handleEscape, { capture: true });
    };
  }, [menuOpen]);

  function togglePin(opName: string) {
    setPinnedNames((current) => {
      const next = current.includes(opName)
        ? current.filter((name) => name !== opName)
        : [...current, opName];
      writePinnedTextTools(next);
      return next;
    });
  }

  const pinnedOperations = pinnedNames
    .map((name) => availableOperations.find((op) => op.spec.name === name))
    .filter((op): op is OperationInfo => Boolean(op));

  if (loadError && availableOperations.length === 0) {
    return (
      <div
        role="toolbar"
        aria-label={m["collections.focusToolbar.label"]()}
        className="mb-4 flex items-center gap-3 rounded-lg border border-hair bg-surface px-3 py-2 text-body-sm text-fg shadow-1"
      >
        <CircleAlert
          className="size-4 shrink-0 text-warn"
          aria-hidden="true"
          strokeWidth={ICON_STROKE}
        />
        <span className="flex-1 text-caption text-fg2">
          {m["collections.textTool.loadError"]()}
        </span>
        <Button type="button" variant="outline" size="xs" onClick={onRetry}>
          {m["collections.textTool.retry"]()}
        </Button>
      </div>
    );
  }

  return (
    <div
      role="toolbar"
      aria-label={m["collections.focusToolbar.label"]()}
      className="mb-4 flex flex-wrap items-center justify-between gap-2 rounded-lg border border-hair bg-surface px-3 py-2 shadow-1"
    >
      <div className="flex flex-wrap items-center gap-1.5">
        <span className="mr-1 font-mono text-caption font-medium text-accent">#{slotId}</span>
        {pinnedOperations.map((operation) => (
          <Button
            key={operation.spec.name}
            type="button"
            variant="ghost"
            size="xs"
            onClick={() => onSelectOperation(operation)}
          >
            <Type aria-hidden="true" strokeWidth={ICON_STROKE} />
            {textOperationLabel(operation.spec.name)}
          </Button>
        ))}
      </div>

      <div className="relative" ref={menuRef}>
        <Button
          type="button"
          variant="ghost"
          size="icon-xs"
          aria-label={m["collections.focusToolbar.pinTools"]()}
          aria-expanded={menuOpen}
          title={m["collections.focusToolbar.pinTools"]()}
          onClick={() => setMenuOpen((open) => !open)}
        >
          <Plus aria-hidden="true" strokeWidth={ICON_STROKE} />
        </Button>

        {menuOpen && (
          <div
            role="menu"
            aria-label={m["collections.focusToolbar.pinMenuTitle"]()}
            className="absolute right-0 z-30 mt-1.5 w-56 rounded-lg border border-hair bg-surface p-1.5 shadow-2"
          >
            <div className="px-2 py-1 text-caption font-medium text-fg2">
              {m["collections.focusToolbar.pinMenuTitle"]()}
            </div>
            {availableOperations.length === 0 ? (
              <div className="px-2 py-1.5 text-caption text-fg3">
                {m["collections.focusToolbar.noTools"]()}
              </div>
            ) : (
              availableOperations.map((operation) => {
                const name = operation.spec.name;
                const pinned = pinnedNames.includes(name);
                return (
                  <button
                    key={name}
                    type="button"
                    role="menuitemcheckbox"
                    aria-checked={pinned}
                    onClick={() => togglePin(name)}
                    className="flex w-full cursor-pointer items-center justify-between gap-2 rounded-md px-2 py-1.5 text-left text-body-sm text-fg transition-colors hover:bg-hover"
                  >
                    <span>{textOperationLabel(name)}</span>
                    {pinned && (
                      <Check
                        className="size-3.5 shrink-0 text-accent"
                        aria-hidden="true"
                        strokeWidth={ICON_STROKE}
                      />
                    )}
                  </button>
                );
              })
            )}
          </div>
        )}
      </div>
    </div>
  );
}
