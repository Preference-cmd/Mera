import { useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import * as m from "$paraglide/messages";
import { useProjectStore } from "@/store/project";
import { Input } from "@/components/ui/input";

const NAME_MAX_LENGTH = 120;

/**
 * Inline rename for a workspace. Blur or Enter commits a changed, non-empty
 * name; Esc reverts to the stored name. An empty or whitespace-only name stays
 * in the field with an inline error and is never sent.
 */
export function WorkspaceNameField({ projectId, name }: { projectId: string; name: string }) {
  const renameProject = useProjectStore((state) => state.renameProject);
  const [draft, setDraft] = useState(name);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  // Esc reverts and then blurs; the blur must not commit the reverted draft again.
  const skipBlurCommit = useRef(false);

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
    setSaving(true);
    try {
      await renameProject(projectId, trimmed);
      setDraft(trimmed);
    } catch (err) {
      toast.error(err instanceof Error ? err.message : m["settings.workspaceRenameFailed"]());
      setDraft(name);
    } finally {
      setSaving(false);
    }
  };

  const errorId = `workspace-name-error-${projectId}`;

  return (
    <div className="flex flex-col gap-1.5">
      <Input
        id="settings-workspace-name"
        data-settings-escape-reverts="true"
        aria-labelledby="setting-workspace-name-label"
        aria-invalid={error ? true : undefined}
        aria-describedby={error ? errorId : undefined}
        value={draft}
        maxLength={NAME_MAX_LENGTH}
        disabled={saving}
        onChange={(event) => {
          setDraft(event.target.value);
          if (error) setError(null);
        }}
        onBlur={() => {
          if (skipBlurCommit.current) {
            skipBlurCommit.current = false;
            return;
          }
          void commit();
        }}
        onKeyDown={(event) => {
          if (event.key === "Enter") {
            event.preventDefault();
            void commit();
          } else if (event.key === "Escape") {
            // The dialog stays open for this Esc (see its onEscapeKeyDown).
            event.preventDefault();
            skipBlurCommit.current = true;
            setDraft(name);
            setError(null);
            event.currentTarget.blur();
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
