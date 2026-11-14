import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { CircleAlert, Plus, Trash2 } from "lucide-react";
import { diffLines } from "diff";
import { toast } from "sonner";
import * as m from "$paraglide/messages";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { opInvoke, opPreview, opPreviewCancel, readAssetText } from "@/ipc";
import {
  CollectionDocumentSchema,
  type CollectionSlot,
  type OperationInfo,
  type RecipeStep,
} from "@/ipc/schemas";
import { getPreviewUrl } from "@/lib/assetUrl";
import { textOperationLabel } from "@/lib/textOperations";

const PREVIEW_DEBOUNCE_MS = 275;

type JsonSchema = {
  type?: string | string[];
  properties?: Record<string, JsonSchema>;
  items?: JsonSchema;
  enum?: unknown[];
  default?: unknown;
  format?: string;
};

type PairRow = {
  key: string;
  values: Record<string, unknown>;
};

type NormalizeValues = {
  whitespace: boolean;
  newline: boolean;
  punctuation: string;
  unicode: string;
};

type TemplateValueRow = {
  id: string;
  key: string;
  value: string;
  fromUpstream: boolean;
  originalKey?: string;
  originalValue?: string;
  rawOriginal?: unknown;
};

/** `pairIndexes` maps each count back to its row in `pairs` after empty rows drop. */
type PreviewResult = { text: string; counts: number[]; pairIndexes: number[] };

let rowSequence = 0;

function nextRowKey(prefix: string): string {
  rowSequence += 1;
  return `${prefix}-${rowSequence}`;
}

/** A preview key is stable per dialog and safe as a directory name. */
function createPreviewKey(): string {
  return `pk_${Date.now().toString(36)}_${Math.random().toString(36).slice(2, 10)}`;
}

function schemaOf(operation: OperationInfo | undefined): JsonSchema {
  return (operation?.spec?.input_schema ?? {}) as JsonSchema;
}

/** Defaults come from the pair item schema so the form follows the operation. */
function defaultPairValues(items: JsonSchema | undefined): Record<string, unknown> {
  const values: Record<string, unknown> = {};
  for (const [name, field] of Object.entries(items?.properties ?? {})) {
    const type = Array.isArray(field.type) ? field.type[0] : field.type;
    if (type === "boolean") {
      values[name] = typeof field.default === "boolean" ? field.default : false;
    } else if (Array.isArray(field.enum)) {
      values[name] = field.default ?? field.enum[0] ?? "";
    } else {
      values[name] = typeof field.default === "string" ? field.default : "";
    }
  }
  if (typeof values.mode !== "string") values.mode = "literal";
  if (typeof values.case_sensitive !== "boolean") values.case_sensitive = true;
  return values;
}

function createPair(items: JsonSchema | undefined, initial?: Record<string, unknown>): PairRow {
  return {
    key: nextRowKey("pair"),
    values: { ...defaultPairValues(items), ...initial },
  };
}

function initialPairs(items: JsonSchema | undefined, step: RecipeStep | undefined): PairRow[] {
  const rawPairs = step?.input?.pairs;
  if (Array.isArray(rawPairs) && rawPairs.length > 0) {
    return rawPairs
      .filter((item): item is Record<string, unknown> => Boolean(item && typeof item === "object"))
      .map((item) => createPair(items, item));
  }
  return [createPair(items)];
}

function defaultNormalizeValues(schema: JsonSchema, step: RecipeStep | undefined): NormalizeValues {
  const props = schema.properties ?? {};
  const input = step?.input ?? {};
  const whitespace =
    typeof input.whitespace === "boolean"
      ? input.whitespace
      : typeof props.whitespace?.default === "boolean"
        ? props.whitespace.default
        : true;
  const newline =
    typeof input.newline === "boolean"
      ? input.newline
      : typeof props.newline?.default === "boolean"
        ? props.newline.default
        : true;
  const punctuation =
    typeof input.punctuation === "string"
      ? input.punctuation
      : typeof props.punctuation?.default === "string"
        ? props.punctuation.default
        : "none";
  const unicode =
    typeof input.unicode === "string"
      ? input.unicode
      : typeof props.unicode?.default === "string"
        ? props.unicode.default
        : "NFC";
  return { whitespace, newline, punctuation, unicode };
}

function initialTemplateText(step: RecipeStep | undefined): string {
  return typeof step?.input?.template === "string" ? step.input.template : "{{body}}";
}

function initialTemplateValues(step: RecipeStep | undefined): TemplateValueRow[] {
  const rawValues =
    step?.input?.values &&
    typeof step.input.values === "object" &&
    !Array.isArray(step.input.values)
      ? (step.input.values as Record<string, unknown>)
      : {};
  const sources = step?.input_sources ?? {};
  const keys = new Set<string>(Object.keys(rawValues));
  for (const pointer of Object.keys(sources)) {
    if (pointer.startsWith("/values/")) {
      const key = pointer.slice("/values/".length);
      if (key) keys.add(key);
    }
  }
  return Array.from(keys).map((key) => {
    const raw = rawValues[key];
    const valueStr = raw === undefined || raw === null ? "" : String(raw);
    const fromUpstream = Boolean(sources[`/values/${key}`]);
    return {
      id: nextRowKey("tpl"),
      key,
      value: valueStr,
      fromUpstream,
      originalKey: key,
      originalValue: valueStr,
      rawOriginal: raw,
    };
  });
}

function buildTemplateValuesPayload(rows: TemplateValueRow[]): Record<string, unknown> {
  const result: Record<string, unknown> = {};
  for (const row of rows) {
    const key = row.key.trim();
    if (!key) continue;
    if (row.fromUpstream && row.originalKey === key && row.value === (row.originalValue ?? "")) {
      if (row.rawOriginal !== undefined) {
        result[key] = row.rawOriginal;
      }
      continue;
    }
    result[key] = row.value;
  }
  return result;
}

function pairValue(pair: PairRow, name: string): unknown {
  return pair.values[name];
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function fieldLabel(name: string): string {
  switch (name) {
    case "find":
      return m["collections.textTool.fields.find"]();
    case "replace":
      return m["collections.textTool.fields.replace"]();
    case "mode":
      return m["collections.textTool.fields.mode"]();
    case "case_sensitive":
      return m["collections.textTool.fields.caseSensitive"]();
    default:
      return name;
  }
}

function modeLabel(mode: string): string {
  switch (mode) {
    case "literal":
      return m["collections.textTool.modes.literal"]();
    case "word":
      return m["collections.textTool.modes.word"]();
    case "regex":
      return m["collections.textTool.modes.regex"]();
    default:
      // A newer host may add a mode this build does not name; show it as sent.
      return mode;
  }
}

function punctuationLabel(value: string): string {
  switch (value) {
    case "none":
      return m["collections.textTool.punctuation.none"]();
    case "halfwidth":
      return m["collections.textTool.punctuation.halfwidth"]();
    default:
      return value;
  }
}

function unicodeLabel(value: string): string {
  switch (value) {
    case "NFC":
      return m["collections.textTool.unicode.NFC"]();
    case "none":
      return m["collections.textTool.unicode.none"]();
    default:
      return value;
  }
}

/**
 * Drop pairs with no find string and remember each kept pair's row index, so a
 * per-pair count still points at the row the user edited.
 */
function pairsForInput(pairs: PairRow[]): {
  pairs: Array<Record<string, unknown>>;
  pairIndexes: number[];
} {
  const kept = pairs
    .map((pair, index) => ({ pair, index }))
    .filter(({ pair }) => String(pairValue(pair, "find") ?? "").length > 0);
  return {
    pairs: kept.map(({ pair }) => ({
      find: String(pairValue(pair, "find") ?? ""),
      replace: String(pairValue(pair, "replace") ?? ""),
      mode: String(pairValue(pair, "mode") ?? "literal"),
      case_sensitive: pairValue(pair, "case_sensitive") !== false,
    })),
    pairIndexes: kept.map(({ index }) => index),
  };
}

/**
 * Resolve a preview output: the mock returns inline `text`, the host returns a
 * `doc` URI that has to be read through the asset protocol.
 */
async function readPreviewOutput(
  output: unknown,
  pairIndexes: number[],
  cacheBuster: number,
  signal: AbortSignal,
): Promise<PreviewResult> {
  if (!output || typeof output !== "object") {
    throw new Error(m["collections.textTool.previewOutputError"]());
  }
  const record = output as { text?: unknown; doc?: unknown; counts?: unknown };
  const counts = Array.isArray(record.counts)
    ? record.counts.map((count) => (typeof count === "number" ? count : 0))
    : [];
  if (typeof record.text === "string") {
    return { text: record.text, counts, pairIndexes };
  }
  if (typeof record.doc === "string") {
    const res = await fetch(getPreviewUrl(record.doc, cacheBuster), { signal });
    if (!res.ok) {
      throw new Error(m["collections.textTool.previewReadError"]({ status: String(res.status) }));
    }
    return { text: await res.text(), counts, pairIndexes };
  }
  throw new Error(m["collections.textTool.previewOutputError"]());
}

function DiffView({ original, changed }: { original: string; changed: string }) {
  const parts = diffLines(original, changed);
  return (
    <pre
      data-testid="text-tool-diff"
      className="max-h-80 overflow-auto rounded-lg bg-tile p-3 font-mono text-caption whitespace-pre-wrap text-fg"
    >
      {parts.map((part, partIndex) => {
        const lines = part.value.split("\n");
        if (lines[lines.length - 1] === "") lines.pop();
        return lines.map((line, lineIndex) => {
          const tone = part.added
            ? "bg-ok/10 text-fg"
            : part.removed
              ? "bg-err/10 text-fg"
              : "text-fg2";
          const marker = part.added ? "+ " : part.removed ? "- " : "  ";
          return (
            <div
              key={`${partIndex}-${lineIndex}`}
              className={tone}
              data-diff={part.added ? "added" : part.removed ? "removed" : "unchanged"}
            >
              <span aria-hidden="true">{marker}</span>
              {line}
            </div>
          );
        });
      })}
    </pre>
  );
}

function Diagnostic({ children }: { children: ReactNode }) {
  return (
    <div className="flex items-start gap-2 rounded-lg border-l-2 border-err bg-err/10 p-3 text-body-sm text-fg">
      <CircleAlert className="mt-0.5 size-4 shrink-0 text-err" aria-hidden="true" />
      <span>{children}</span>
    </div>
  );
}

export function TextToolDialog({
  open,
  projectId,
  collectionId,
  slot,
  operation,
  mode = "append",
  step,
  rootAssetId,
  onOpenChange,
  onApplied,
}: {
  open: boolean;
  projectId: string;
  collectionId: string;
  slot: CollectionSlot;
  operation: OperationInfo | undefined;
  mode?: "append" | "edit";
  step?: RecipeStep;
  rootAssetId?: string;
  onOpenChange: (open: boolean) => void;
  onApplied?: () => void | Promise<void>;
}) {
  const opName = operation?.spec?.name ?? step?.op ?? "text.replace";
  const inputSchema = schemaOf(operation);
  const pairsSchema = inputSchema.properties?.pairs;
  const pairItems = pairsSchema?.items;

  const baseAssetId =
    mode === "edit"
      ? typeof step?.input?.doc === "string"
        ? step.input.doc
        : (rootAssetId ?? slot.asset_id)
      : slot.asset_id;

  const [pairs, setPairs] = useState<PairRow[]>(() => initialPairs(pairItems, step));
  const [normalizeValues, setNormalizeValues] = useState<NormalizeValues>(() =>
    defaultNormalizeValues(inputSchema, step),
  );
  const [templateText, setTemplateText] = useState<string>(() => initialTemplateText(step));
  const [templateValues, setTemplateValues] = useState<TemplateValueRow[]>(() =>
    initialTemplateValues(step),
  );

  const [original, setOriginal] = useState<string | null>(null);
  const [originalError, setOriginalError] = useState<string | null>(null);
  const [preview, setPreview] = useState<PreviewResult | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [previewing, setPreviewing] = useState(false);
  const [applying, setApplying] = useState(false);
  const [applyError, setApplyError] = useState<string | null>(null);

  const previewKeyRef = useRef(createPreviewKey());
  const previewSequence = useRef(0);
  const previewInFlightRef = useRef(false);
  const previewAbortRef = useRef<AbortController | null>(null);
  const applyingRef = useRef(false);

  /**
   * Stop the host preview that is still running for this dialog. The response
   * is ignored either way through `previewSequence`; the abort and cancel just
   * stop work the user no longer sees.
   */
  const cancelPreviewInFlight = useCallback(() => {
    previewAbortRef.current?.abort();
    previewAbortRef.current = null;
    if (!previewInFlightRef.current || applyingRef.current) return;
    previewInFlightRef.current = false;
    void opPreviewCancel(previewKeyRef.current).catch(() => {
      // Closing must not fail on a host that already dropped the preview.
    });
  }, []);

  // A direct unmount (route change, parent reset) must not leave a preview
  // running either.
  useEffect(() => () => cancelPreviewInFlight(), [cancelPreviewInFlight]);

  // A new open starts from a fresh preview key and initializes form state from `step` (in edit mode) or defaults.
  useEffect(() => {
    if (!open) {
      previewSequence.current += 1;
      return;
    }
    previewSequence.current += 1;
    cancelPreviewInFlight();
    previewKeyRef.current = createPreviewKey();
    setPairs(initialPairs(pairItems, mode === "edit" ? step : undefined));
    setNormalizeValues(defaultNormalizeValues(inputSchema, mode === "edit" ? step : undefined));
    setTemplateText(initialTemplateText(mode === "edit" ? step : undefined));
    setTemplateValues(initialTemplateValues(mode === "edit" ? step : undefined));
    setOriginal(null);
    setOriginalError(null);
    setPreview(null);
    setPreviewError(null);
    setPreviewing(false);
    setApplying(false);
    setApplyError(null);
  }, [
    open,
    projectId,
    baseAssetId,
    opName,
    mode,
    step,
    pairItems,
    inputSchema,
    cancelPreviewInFlight,
  ]);

  useEffect(() => {
    if (!open) return;
    let active = true;
    readAssetText(projectId, baseAssetId).then(
      (text) => {
        if (active) setOriginal(text);
      },
      (error: unknown) => {
        if (active) setOriginalError(errorMessage(error));
      },
    );
    return () => {
      active = false;
    };
  }, [open, projectId, baseAssetId]);

  const validPairs = pairsForInput(pairs);
  const hasValidInput =
    opName === "text.replace"
      ? validPairs.pairs.length > 0
      : opName === "text.template"
        ? templateText.length > 0
        : opName === "text.normalize";

  function buildOpParams(): Record<string, unknown> {
    if (opName === "text.replace") {
      return { pairs: validPairs.pairs };
    }
    if (opName === "text.normalize") {
      return { ...normalizeValues };
    }
    if (opName === "text.template") {
      return {
        template: templateText,
        values: buildTemplateValuesPayload(templateValues),
      };
    }
    return {};
  }

  useEffect(() => {
    const sequence = ++previewSequence.current;
    // Every pass owns the flag: a response from an earlier edit must not claim
    // (or clear) the in-flight state of a request this pass has not sent yet.
    previewInFlightRef.current = false;
    if (!open || original === null || !hasValidInput) {
      setPreview(null);
      setPreviewError(null);
      setPreviewing(false);
      return;
    }
    setPreviewing(true);
    setPreviewError(null);
    let controller: AbortController | null = null;
    const params = buildOpParams();
    const pairIndexes = opName === "text.replace" ? validPairs.pairIndexes : [];
    const timer = setTimeout(() => {
      const current = new AbortController();
      controller = current;
      previewAbortRef.current = current;
      void (async () => {
        previewInFlightRef.current = true;
        try {
          const output = await opPreview(
            projectId,
            opName,
            { doc: baseAssetId, ...params },
            previewKeyRef.current,
          );
          if (sequence !== previewSequence.current) return;
          const result = await readPreviewOutput(output, pairIndexes, sequence, current.signal);
          if (sequence !== previewSequence.current) return;
          setPreview(result);
          setPreviewing(false);
        } catch (error) {
          if (sequence !== previewSequence.current) return;
          setPreview(null);
          setPreviewError(errorMessage(error));
          setPreviewing(false);
        } finally {
          if (previewAbortRef.current === current) previewAbortRef.current = null;
          if (sequence === previewSequence.current) previewInFlightRef.current = false;
        }
      })();
    }, PREVIEW_DEBOUNCE_MS);

    return () => {
      clearTimeout(timer);
      controller?.abort();
      if (previewAbortRef.current === controller) previewAbortRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    open,
    original,
    pairs,
    normalizeValues,
    templateText,
    templateValues,
    projectId,
    baseAssetId,
    opName,
    hasValidInput,
  ]);

  function updatePair(key: string, name: string, value: unknown) {
    setPairs((current) =>
      current.map((pair) =>
        pair.key === key ? { ...pair, values: { ...pair.values, [name]: value } } : pair,
      ),
    );
  }

  function addPair() {
    setPairs((current) => [...current, createPair(pairItems)]);
  }

  function removePair(key: string) {
    setPairs((current) => current.filter((pair) => pair.key !== key));
  }

  function addTemplateValue() {
    setTemplateValues((current) => [
      ...current,
      { id: nextRowKey("tpl"), key: "", value: "", fromUpstream: false },
    ]);
  }

  function updateTemplateValue(id: string, field: "key" | "value", nextVal: string) {
    setTemplateValues((current) =>
      current.map((row) => (row.id === id ? { ...row, [field]: nextVal } : row)),
    );
  }

  function removeTemplateValue(id: string) {
    setTemplateValues((current) => current.filter((row) => row.id !== id));
  }

  function handleOpenChange(next: boolean) {
    if (!next) {
      // Escape, the overlay and the close button all arrive here. An apply in
      // progress owns the dialog until it succeeds or fails.
      if (applyingRef.current) return;
      previewSequence.current += 1;
      cancelPreviewInFlight();
    }
    onOpenChange(next);
  }

  const canApply =
    hasValidInput &&
    preview !== null &&
    !previewing &&
    !applying &&
    previewError === null &&
    originalError === null;

  async function handleApply() {
    if (!canApply) return;
    applyingRef.current = true;
    setApplying(true);
    setApplyError(null);
    const params = buildOpParams();
    try {
      if (mode === "edit" && step) {
        await opInvoke(projectId, "recipe.edit", {
          collection_id: collectionId,
          slot_id: slot.id,
          step_id: step.step_id,
          input: { doc: baseAssetId, ...params },
        });
        toast.success(m["collections.textTool.editApplied"]());
      } else {
        const current = await opInvoke(projectId, "collection.get", {
          collection_id: collectionId,
        });
        const document = CollectionDocumentSchema.parse(current.output);
        const currentSlot = document.slots.find((candidate) => candidate.id === slot.id);
        if (!currentSlot || currentSlot.asset_id !== baseAssetId) {
          setApplyError(m["collections.textTool.slotChanged"]());
          return;
        }

        const invoked = await opInvoke(projectId, opName, {
          doc: baseAssetId,
          ...params,
        });
        const output = (invoked.output ?? null) as { doc?: unknown } | null;
        if (!output || typeof output.doc !== "string") {
          setApplyError(m["collections.textTool.inlineOutput"]());
          return;
        }

        await opInvoke(projectId, "collection.set_slot", {
          collection_id: collectionId,
          slot_id: slot.id,
          asset: output.doc,
        });
        toast.success(m["collections.textTool.applied"]());
      }

      try {
        await onApplied?.();
      } catch {
        // The collection.changed event still refreshes an open view.
      }
      // The write is done, so this close is allowed through the guard.
      applyingRef.current = false;
      handleOpenChange(false);
    } catch (error) {
      setApplyError(`${m["collections.textTool.applyError"]()}: ${errorMessage(error)}`);
    } finally {
      applyingRef.current = false;
      setApplying(false);
    }
  }

  const unchanged = original !== null && preview !== null && preview.text === original;
  const previewText = preview?.text ?? original ?? "";

  const operationMissingSchema =
    opName === "text.replace" ? !pairsSchema || !pairItems : !operation;

  const dialogTitle =
    mode === "edit"
      ? `${m["collections.textTool.editTitle"]()} · ${textOperationLabel(opName)}`
      : textOperationLabel(opName);
  const dialogDescription =
    mode === "edit"
      ? m["collections.textTool.editDescription"]()
      : m["collections.textTool.description"]();

  const punctuationOptions = Array.isArray(inputSchema.properties?.punctuation?.enum)
    ? inputSchema.properties.punctuation.enum.map(String)
    : ["none", "halfwidth"];
  const unicodeOptions = Array.isArray(inputSchema.properties?.unicode?.enum)
    ? inputSchema.properties.unicode.enum.map(String)
    : ["NFC", "none"];

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent className="max-h-[85vh] overflow-y-auto sm:max-w-4xl">
        <DialogHeader>
          <DialogTitle>{dialogTitle}</DialogTitle>
          <DialogDescription>
            {dialogDescription} ({m["collections.slotPrefix"]()} {slot.id})
          </DialogDescription>
        </DialogHeader>

        {originalError ? (
          <Diagnostic>
            {m["collections.textTool.originalError"]()}: {originalError}
          </Diagnostic>
        ) : original === null ? (
          <div className="text-body-sm text-fg2">{m["collections.textTool.loadingOriginal"]()}</div>
        ) : operationMissingSchema ? (
          <div className="text-body-sm text-fg2">{m["collections.textTool.unavailable"]()}</div>
        ) : (
          <div className="grid gap-4 md:grid-cols-[minmax(0,1fr)_minmax(0,1fr)]">
            {opName === "text.replace" && (
              <section
                className="flex flex-col gap-3"
                aria-label={m["collections.textTool.fields.pairs"]()}
              >
                <div className="flex items-center justify-between">
                  <h3 className="text-body-sm font-medium text-fg">
                    {m["collections.textTool.fields.pairs"]()}
                  </h3>
                  <Button type="button" variant="outline" size="sm" onClick={addPair}>
                    <Plus aria-hidden="true" />
                    {m["collections.textTool.addPair"]()}
                  </Button>
                </div>

                {pairs.length === 0 && (
                  <p className="text-caption text-fg2">{m["collections.textTool.noPairs"]()}</p>
                )}

                {pairs.map((pair, index) => (
                  <div
                    key={pair.key}
                    className="rounded-lg border border-hair bg-ground p-3"
                    data-testid="text-tool-pair"
                  >
                    <div className="mb-2 flex items-center justify-between">
                      <span className="text-caption font-medium text-fg2">
                        {m["collections.textTool.pairLabel"]({ index: index + 1 })}
                      </span>
                      <Button
                        type="button"
                        variant="ghost"
                        size="icon-xs"
                        aria-label={m["collections.textTool.removePair"]()}
                        onClick={() => removePair(pair.key)}
                      >
                        <Trash2 aria-hidden="true" />
                      </Button>
                    </div>

                    <div className="flex flex-col gap-2">
                      {Object.entries(pairItems?.properties ?? {}).map(([name, field]) => {
                        const id = `text-tool-${pair.key}-${name.replace(/[^a-zA-Z0-9_-]/g, "-")}`;
                        const type = Array.isArray(field.type) ? field.type[0] : field.type;
                        if (Array.isArray(field.enum)) {
                          return (
                            <div key={name} className="flex flex-col gap-2">
                              <label htmlFor={id} className="text-caption text-fg2">
                                {fieldLabel(name)}
                              </label>
                              <select
                                id={id}
                                value={String(pairValue(pair, name) ?? "")}
                                onChange={(event) => updatePair(pair.key, name, event.target.value)}
                                className="h-9 w-full rounded-md border border-hair bg-surface px-3 text-sm text-fg outline-none focus-visible:border-accent focus-visible:ring-[3px] focus-visible:ring-accent/25"
                              >
                                {field.enum.map((option) => (
                                  <option key={String(option)} value={String(option)}>
                                    {name === "mode" ? modeLabel(String(option)) : String(option)}
                                  </option>
                                ))}
                              </select>
                            </div>
                          );
                        }
                        if (type === "boolean") {
                          return (
                            <label
                              key={name}
                              htmlFor={id}
                              className="mt-1 flex items-center gap-2 text-body-sm text-fg"
                            >
                              <input
                                id={id}
                                type="checkbox"
                                checked={pairValue(pair, name) !== false}
                                onChange={(event) =>
                                  updatePair(pair.key, name, event.target.checked)
                                }
                                className="size-4 accent-accent"
                              />
                              {fieldLabel(name)}
                            </label>
                          );
                        }
                        if (type === "string" || type === undefined) {
                          return (
                            <div key={name} className="flex flex-col gap-2">
                              <label htmlFor={id} className="text-caption text-fg2">
                                {fieldLabel(name)}
                              </label>
                              <Input
                                id={id}
                                value={String(pairValue(pair, name) ?? "")}
                                onChange={(event) => updatePair(pair.key, name, event.target.value)}
                              />
                            </div>
                          );
                        }
                        return null;
                      })}
                    </div>
                  </div>
                ))}
              </section>
            )}

            {opName === "text.normalize" && (
              <section
                className="flex flex-col gap-3"
                aria-label={textOperationLabel("text.normalize")}
              >
                <div className="flex flex-col gap-3 rounded-lg border border-hair bg-ground p-3">
                  <label
                    htmlFor="text-normalize-whitespace"
                    className="flex items-center gap-2 text-body-sm text-fg"
                  >
                    <input
                      id="text-normalize-whitespace"
                      type="checkbox"
                      checked={normalizeValues.whitespace}
                      onChange={(event) =>
                        setNormalizeValues((curr) => ({
                          ...curr,
                          whitespace: event.target.checked,
                        }))
                      }
                      className="size-4 accent-accent"
                    />
                    {m["collections.textTool.fields.whitespace"]()}
                  </label>

                  <label
                    htmlFor="text-normalize-newline"
                    className="flex items-center gap-2 text-body-sm text-fg"
                  >
                    <input
                      id="text-normalize-newline"
                      type="checkbox"
                      checked={normalizeValues.newline}
                      onChange={(event) =>
                        setNormalizeValues((curr) => ({ ...curr, newline: event.target.checked }))
                      }
                      className="size-4 accent-accent"
                    />
                    {m["collections.textTool.fields.newline"]()}
                  </label>

                  <div className="flex flex-col gap-1.5">
                    <label htmlFor="text-normalize-punctuation" className="text-caption text-fg2">
                      {m["collections.textTool.fields.punctuation"]()}
                    </label>
                    <select
                      id="text-normalize-punctuation"
                      value={normalizeValues.punctuation}
                      onChange={(event) =>
                        setNormalizeValues((curr) => ({
                          ...curr,
                          punctuation: event.target.value,
                        }))
                      }
                      className="h-9 w-full rounded-md border border-hair bg-surface px-3 text-sm text-fg outline-none focus-visible:border-accent focus-visible:ring-[3px] focus-visible:ring-accent/25"
                    >
                      {punctuationOptions.map((option) => (
                        <option key={option} value={option}>
                          {punctuationLabel(option)}
                        </option>
                      ))}
                    </select>
                  </div>

                  <div className="flex flex-col gap-1.5">
                    <label htmlFor="text-normalize-unicode" className="text-caption text-fg2">
                      {m["collections.textTool.fields.unicode"]()}
                    </label>
                    <select
                      id="text-normalize-unicode"
                      value={normalizeValues.unicode}
                      onChange={(event) =>
                        setNormalizeValues((curr) => ({ ...curr, unicode: event.target.value }))
                      }
                      className="h-9 w-full rounded-md border border-hair bg-surface px-3 text-sm text-fg outline-none focus-visible:border-accent focus-visible:ring-[3px] focus-visible:ring-accent/25"
                    >
                      {unicodeOptions.map((option) => (
                        <option key={option} value={option}>
                          {unicodeLabel(option)}
                        </option>
                      ))}
                    </select>
                  </div>
                </div>
              </section>
            )}

            {opName === "text.template" && (
              <section
                className="flex flex-col gap-3"
                aria-label={textOperationLabel("text.template")}
              >
                <div className="flex flex-col gap-1.5">
                  <label htmlFor="text-template-body" className="text-body-sm font-medium text-fg">
                    {m["collections.textTool.fields.template"]()}
                  </label>
                  <textarea
                    id="text-template-body"
                    rows={6}
                    value={templateText}
                    onChange={(event) => setTemplateText(event.target.value)}
                    className="w-full rounded-md border border-hair bg-surface p-3 font-mono text-caption text-fg outline-none focus-visible:border-accent focus-visible:ring-[3px] focus-visible:ring-accent/25"
                  />
                </div>

                <div className="flex items-center justify-between">
                  <h3 className="text-body-sm font-medium text-fg">
                    {m["collections.textTool.fields.values"]()}
                  </h3>
                  <Button type="button" variant="outline" size="sm" onClick={addTemplateValue}>
                    <Plus aria-hidden="true" />
                    {m["collections.textTool.addValue"]()}
                  </Button>
                </div>

                {templateValues.length === 0 ? (
                  <p className="text-caption text-fg2">{m["collections.textTool.noValues"]()}</p>
                ) : (
                  <div className="flex flex-col gap-2">
                    {templateValues.map((row) => (
                      <div
                        key={row.id}
                        className="flex flex-col gap-2 rounded-lg border border-hair bg-ground p-2.5"
                      >
                        <div className="flex items-center justify-between gap-2">
                          {row.fromUpstream ? (
                            <span className="rounded border border-agent-line bg-agent-soft px-1.5 py-0.5 text-caption text-agent">
                              {m["collections.textTool.fromUpstream"]()}
                            </span>
                          ) : (
                            <span />
                          )}
                          <Button
                            type="button"
                            variant="ghost"
                            size="icon-xs"
                            aria-label={m["collections.textTool.removeValue"]()}
                            onClick={() => removeTemplateValue(row.id)}
                          >
                            <Trash2 aria-hidden="true" />
                          </Button>
                        </div>
                        <div className="grid grid-cols-2 gap-2">
                          <Input
                            aria-label={m["collections.textTool.valueKey"]()}
                            placeholder={m["collections.textTool.valueKey"]()}
                            value={row.key}
                            onChange={(event) =>
                              updateTemplateValue(row.id, "key", event.target.value)
                            }
                          />
                          <Input
                            aria-label={m["collections.textTool.valueVal"]()}
                            placeholder={m["collections.textTool.valueVal"]()}
                            value={row.value}
                            onChange={(event) =>
                              updateTemplateValue(row.id, "value", event.target.value)
                            }
                          />
                        </div>
                      </div>
                    ))}
                  </div>
                )}
              </section>
            )}

            <section
              className="flex min-w-0 flex-col gap-2"
              aria-label={m["collections.textTool.preview"]()}
            >
              <div className="flex items-center justify-between">
                <h3 className="text-body-sm font-medium text-fg">
                  {m["collections.textTool.preview"]()}
                </h3>
                {previewing && (
                  <span className="text-caption text-fg2">
                    {m["collections.textTool.previewing"]()}
                  </span>
                )}
              </div>

              {!hasValidInput ? (
                <>
                  <p className="text-caption text-fg2">
                    {opName === "text.template"
                      ? m["collections.textTool.emptyTemplate"]()
                      : m["collections.textTool.noPairs"]()}
                  </p>
                  <pre className="max-h-80 overflow-auto rounded-lg bg-tile p-3 font-mono text-caption whitespace-pre-wrap text-fg2">
                    {original}
                  </pre>
                </>
              ) : previewError ? (
                <Diagnostic>
                  {m["collections.textTool.previewError"]()}: {previewError}
                </Diagnostic>
              ) : preview ? (
                <>
                  {unchanged && (
                    <p className="text-caption text-fg2">{m["collections.textTool.noChanges"]()}</p>
                  )}
                  <DiffView original={original} changed={previewText} />
                  {opName === "text.replace" && preview.counts.length > 0 && (
                    <ul className="flex flex-col gap-1 text-caption text-fg2">
                      {preview.counts.map((count, index) => (
                        <li key={preview.pairIndexes[index] ?? index}>
                          {m["collections.textTool.pairCount"]({
                            index: (preview.pairIndexes[index] ?? index) + 1,
                            count,
                          })}
                        </li>
                      ))}
                    </ul>
                  )}
                </>
              ) : (
                <p className="text-caption text-fg2">{m["collections.textTool.emptyPreview"]()}</p>
              )}
            </section>
          </div>
        )}

        {applyError && <Diagnostic>{applyError}</Diagnostic>}

        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            onClick={() => handleOpenChange(false)}
            disabled={applying}
          >
            {m["common.cancel"]()}
          </Button>
          <Button type="button" onClick={() => void handleApply()} disabled={!canApply}>
            {applying ? m["collections.textTool.applying"]() : m["collections.textTool.apply"]()}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
