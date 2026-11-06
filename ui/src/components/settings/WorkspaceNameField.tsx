import { useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import * as m from "$paraglide/messages";
import { useProjectStore } from "@/store/project";
import { Input } from "@/components/ui/input";

const NAME_MAX_LENGTH = 120;

/**
 * Inline rename for a workspace. Blur or Enter commits a changed, non-empty
 * name; Esc reverts an edited draft to the stored name and keeps focus, and
 * Esc on an unedited field closes the dialog. An empty or whitespace-only name
 * stays in the field with an inline error and is never sent.
 */
export function WorkspaceNameField({ projectId, name }: { projectId: string; name: string }) {
  const renameProject = useProjectStore((state) => state.renameProject);
  const [draft, setDraft] = useState(name);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  // Enter and the blur that may follow it must not send the rename twice.
  const inFlight = useRef(false);

  // Follow renames from elsewhere and workspace changes.
  useEffect(() => {
    setDraft(name);
    setError(null);
  }, [name, projectId]);

  const commit = async () => {
    const trimmed = draft.trim();
    if (!trimmed) {
      setError(m["settings.workspaceNameEmpty"]());
      return;
    }
    setError(null);
    if (trimmed === name) {
      setDraft(name);
      return;
    }
    if (inFlight.current) return;
    inFlight.current = true;
    setSaving(true);
    try {
      await renameProject(projectId, trimmed);
      setDraft(trimmed);
    } catch (err) {
      toast.error(err instanceof Error ? err.message : m["settings.workspaceRenameFailed"]());
      setDraft(name);
    } finally {
      inFlight.current = false;
      setSaving(false);
    }
  };

  const errorId = `workspace-name-error-${projectId}`;
  const dirty = draft !== name;

  return (
    <div className="flex flex-col gap-1.5">
      <Input
        id="settings-workspace-name"
        data-settings-escape-reverts={dirty || error ? "true" : undefined}
        aria-labelledby="setting-workspace-name-label"
        aria-invalid={error ? true : undefined}
        aria-describedby={error ? errorId : undefined}
        value={draft}
        maxLength={NAME_MAX_LENGTH}
        readOnly={saving}
        aria-busy={saving || undefined}
        onChange={(event) => {
          setDraft(event.target.value);
          if (error) setError(null);
        }}
        onBlur={() => void commit()}
        onKeyDown={(event) => {
          if (event.key === "Enter") {
            event.preventDefault();
            void commit();
          } else if (event.key === "Escape" && (dirty || error)) {
            // Revert in place; the dialog stays open for this Esc (see its
            // onEscapeKeyDown). An unedited field lets Esc close the dialog.
            setDraft(name);
            setError(null);
          }
        }}
        className="w-[260px] text-ui"
      />
      {error && (
        <p id={errorId} role="alert" className="text-caption text-err">
          {error}
        </p>
      )}
    </div>
  );
}
