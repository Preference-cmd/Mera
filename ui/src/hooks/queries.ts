import { useQuery } from "@tanstack/react-query";
import { scriptApprovalsSnapshot, storageStatus } from "@/ipc";

/* ─── Query keys ─────────────────────────────────────────── */

export const queryKeys = {
  storageStatus: ["storage-status"] as const,
  scriptApprovals: (projectId: string) => ["script-approvals", projectId] as const,
} as const;

/* ─── Queries ────────────────────────────────────────────── */

/** Storage budget status — shared by AppShell banners and Storage settings. */
export function useStorageStatus() {
  return useQuery({
    queryKey: queryKeys.storageStatus,
    queryFn: storageStatus,
    staleTime: 30_000,
  });
}

/** Pending & persisted `script.python` approvals for a workspace project. */
export function useScriptApprovals(projectId: string) {
  return useQuery({
    queryKey: queryKeys.scriptApprovals(projectId),
    queryFn: () => scriptApprovalsSnapshot(projectId),
    enabled: Boolean(projectId),
    staleTime: 10_000,
  });
}
