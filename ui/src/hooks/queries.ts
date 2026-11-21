import { useQuery } from "@tanstack/react-query";
import { storageStatus } from "@/ipc";

/* ─── Query keys ─────────────────────────────────────────── */

export const queryKeys = {
  storageStatus: ["storage-status"] as const,
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
