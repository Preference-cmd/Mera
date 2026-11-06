import * as m from "$paraglide/messages";
import { SettingRow, SettingsCard } from "./SettingRow";
import { WorkspaceNameField } from "./WorkspaceNameField";

export function WorkspaceGeneralSection({ projectId, name }: { projectId: string; name: string }) {
  return (
    <SettingsCard>
      <SettingRow
        labelId="setting-workspace-name-label"
        label={m["settings.workspaceName"]()}
        description={m["settings.workspaceNameDesc"]()}
      >
        <WorkspaceNameField projectId={projectId} name={name} />
      </SettingRow>
    </SettingsCard>
  );
}
