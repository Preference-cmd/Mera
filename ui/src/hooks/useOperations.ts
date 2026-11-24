import { useCallback, useEffect, useMemo, useState } from "react";
import { opList, subscribeEvents } from "@/ipc";
import type { OperationInfo } from "@/ipc/schemas";

/** One automatic retry covers a host that is still starting its mods. */
const OPERATIONS_RETRY_DELAY_MS = 400;

/** Kernel operation namespaces reserved by the host (never treated as external mods). */
const KERNEL_OPERATION_NAMESPACES = new Set([
  "project",
  "board",
  "collection",
  "dynamic_workflow",
  "recipe",
  "job",
  "asset",
  "mod",
  "library",
  "script",
]);

/**
 * Extract the owning mod id from `<mod_id>.<operation>`, or `null` when the
 * operation belongs to a reserved kernel namespace or lacks a namespace prefix.
 */
export function extractModNamespace(opName: string): string | null {
  const dot = opName.indexOf(".");
  if (dot <= 0) return null;
  const namespace = opName.slice(0, dot);
  return KERNEL_OPERATION_NAMESPACES.has(namespace) ? null : namespace;
}

/**
 * A descriptor is usable unless the host marked it unavailable. The health
 * shape is `{ status: "available" }` or `{ status: "unavailable", ... }`;
 * unknown shapes are treated as available so a newer host does not hide tools.
 */
export function isOperationAvailable(operation: OperationInfo | undefined): boolean {
  if (!operation) return false;
  const health = operation.health as { status?: unknown; state?: unknown } | null | undefined;
  if (!health || typeof health !== "object") return true;
  const status = health.status ?? health.state;
  return status === undefined || status === "available";
}

function isAssetDocProperty(prop: unknown, requireTextMediaType: boolean): boolean {
  if (!prop || typeof prop !== "object") return false;
  const record = prop as Record<string, unknown>;
  if (record.format !== "asset") return false;
  if (!requireTextMediaType) return true;
  if (record.contentMediaType === undefined) return true;
  return typeof record.contentMediaType === "string" && record.contentMediaType.startsWith("text/");
}

/**
 * True when an operation is an available, non-read-only single-text-document
 * transform (`doc` asset in, `doc` asset out).
 */
export function isSingleTextDocumentOperation(operation: OperationInfo | undefined): boolean {
  if (!operation || !isOperationAvailable(operation)) return false;
  if (!operation.spec?.name || extractModNamespace(operation.spec.name) === null) return false;
  const annotations = operation.spec?.annotations as Record<string, unknown> | undefined;
  if (annotations?.read_only === true) return false;
  const inputProps = (
    operation.spec?.input_schema as { properties?: Record<string, unknown> } | undefined
  )?.properties;
  const outputProps = (
    operation.spec?.output_schema as { properties?: Record<string, unknown> } | undefined
  )?.properties;
  return isAssetDocProperty(inputProps?.doc, true) && isAssetDocProperty(outputProps?.doc, false);
}

export type OperationsState = {
  operations: OperationInfo[];
  loading: boolean;
  error: string | null;
  textOperations: OperationInfo[];
  textReplace: OperationInfo | undefined;
  textReplaceAvailable: boolean;
  /** Reload `op_list`; the caller surfaces this when the automatic retry fails. */
  retry: () => void;
};

/**
 * Load `op_list` for a project; the host reports mod loading failures there.
 *
 * A failed read is retried once, because the host may still be starting its
 * mods. A second failure is exposed through `error`, and `retry` lets the caller
 * offer another attempt instead of hiding the tools forever.
 */
export function useOperations(projectId: string): OperationsState {
  const [operations, setOperations] = useState<OperationInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [reloadToken, setReloadToken] = useState(0);

  useEffect(() => {
    let active = true;
    let retried = false;
    let retryTimer: ReturnType<typeof setTimeout> | undefined;

    setLoading(true);
    setError(null);

    const load = () => {
      opList(projectId).then(
        (list) => {
          if (!active) return;
          setOperations(Array.isArray(list) ? list : []);
          setLoading(false);
        },
        (err: unknown) => {
          if (!active) return;
          if (!retried) {
            retried = true;
            retryTimer = setTimeout(load, OPERATIONS_RETRY_DELAY_MS);
            return;
          }
          setOperations([]);
          setError(err instanceof Error ? err.message : String(err));
          setLoading(false);
        },
      );
    };
    load();

    return () => {
      active = false;
      if (retryTimer !== undefined) clearTimeout(retryTimer);
    };
  }, [projectId, reloadToken]);

  useEffect(() => {
    if (!projectId) return;
    const unsubscribe = subscribeEvents((event) => {
      if (
        (event.type === "document" &&
          event.projectId === projectId &&
          event.kind === "dynamic_workflow.changed") ||
        (event.type === "mod" &&
          event.kind.startsWith("mod.") &&
          event.kind !== "mod.visibility_changed")
      ) {
        setReloadToken((token) => token + 1);
      }
    });
    return () => unsubscribe();
  }, [projectId]);

  const retry = useCallback(() => setReloadToken((token) => token + 1), []);

  const textOperations = useMemo(
    () => operations.filter((operation) => isSingleTextDocumentOperation(operation)),
    [operations],
  );
  const textReplace = useMemo(
    () => operations.find((operation) => operation.spec?.name === "text.replace"),
    [operations],
  );
  const textReplaceAvailable = isOperationAvailable(textReplace);

  return {
    operations,
    loading,
    error,
    textOperations,
    textReplace,
    textReplaceAvailable,
    retry,
  };
}
