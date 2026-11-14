import {
  type ActivityRecord,
  type BoardCommandResultDto,
  type BoardItem,
  type BoardSnapshot,
  type Event,
  type Project,
  type ProjectMetadataInput,
  type DownloadHuggingfaceModelArgs,
  type DownloadProgress,
  type ModelCard,
  type ModelCatalogEntry,
  type ModelDownloadOutput,
  type ModelInfo,
  type InvokeResult,
  type OperationInfo,
} from "./schemas";

const delay = (ms: number) => new Promise((r) => setTimeout(r, ms));

function rand(prefix: string) {
  return `${prefix}_${Math.random().toString(36).slice(2, 10)}`;
}

const MOCK_MODELS: ModelInfo[] = [
  {
    id: "sd_xl_base_1_0",
    displayName: "Stable Diffusion Xl Base",
    modelSeries: "stable-diffusion-xl",
    variant: "base",
    roles: ["checkpoint-bundle", "diffusion-model"],
    format: "safetensors",
    sourceStatus: "available",
    sizeBytes: 6_940_000_000,
  },
  {
    id: "dreamshaper_8",
    displayName: "Stable Diffusion 1.5 Dreamshaper",
    modelSeries: "stable-diffusion-1.5",
    variant: "dreamshaper",
    roles: ["checkpoint-bundle"],
    format: "safetensors",
    sourceStatus: "available",
    sizeBytes: 2_070_000_000,
  },
];

/* ───── Project mocks (localStorage-backed) ───── */

const PROJECTS_KEY = "reimagine.mock.projects";
const eventSubscribers = new Set<(event: Event) => void>();

function emitEvent(event: Event): void {
  for (const subscriber of eventSubscribers) subscriber(event);
}

function mockReadProjects(): Record<string, Project> {
  const fallback: Project = {
    id: "default",
    name: "Default",
    description: "Default workspace project",
    createdAt: "2026-08-18T00:00:00Z",
    updatedAt: "2026-08-18T00:00:00Z",
  };
  try {
    const parsed = JSON.parse(localStorage.getItem(PROJECTS_KEY) ?? "{}");
    return Object.keys(parsed).length > 0 ? parsed : { default: fallback };
  } catch {
    return { default: fallback };
  }
}

function mockWriteProjects(projects: Record<string, Project>): void {
  localStorage.setItem(PROJECTS_KEY, JSON.stringify(projects));
}

export async function mockSubscribeEvents(onEvent: (event: Event) => void): Promise<void> {
  eventSubscribers.add(onEvent);
}

export async function mockListProjects(): Promise<Project[]> {
  await delay(30);
  return Object.values(mockReadProjects());
}

export async function mockCreateProject(input: {
  projectId: string;
  metadata: ProjectMetadataInput;
}): Promise<Project> {
  const projects = mockReadProjects();
  const now = new Date().toISOString();
  const project: Project = {
    id: input.projectId,
    name: input.metadata.name,
    description: input.metadata.description,
    createdAt: input.metadata.createdAt ?? now,
    updatedAt: input.metadata.updatedAt ?? now,
  };
  projects[input.projectId] = project;
  mockWriteProjects(projects);
  return project;
}

export async function mockLoadProject(input: { projectId: string }): Promise<Project> {
  const project = mockReadProjects()[input.projectId];
  if (!project) throw new Error("mock project not found: " + input.projectId);
  return project;
}

export async function mockDeleteProject(input: { projectId: string }): Promise<void> {
  const projects = mockReadProjects();
  delete projects[input.projectId];
  mockWriteProjects(projects);
  mockResetBoard(input.projectId);
}

export async function mockSetActiveProject(input: { projectId: string }): Promise<Project> {
  return mockLoadProject(input);
}

/** One undoable board batch: the items before it, its history id and targets. */
type MockBoardHistoryEntry = {
  items: BoardItem[];
  entryId: string;
  targets: string[];
};

const mockBoards: Record<string, BoardSnapshot> = {};
const mockBoardHistory: Record<
  string,
  {
    past: MockBoardHistoryEntry[];
    future: MockBoardHistoryEntry[];
  }
> = {};

function mockBoardEntryId(
  history: { past: MockBoardHistoryEntry[]; future: MockBoardHistoryEntry[] } | undefined,
  direction: "past" | "future",
): string | null {
  const entries = history?.[direction];
  if (!entries || entries.length === 0) return null;
  return entries[entries.length - 1].entryId;
}

/**
 * Mirrors the host's history guard: reject before touching the board, the
 * history stacks or the activity feed. The details carry both current tops so
 * the UI can resynchronize its stacks.
 */
function rejectBoardHistoryMismatch(
  projectId: string,
  action: "undo" | "redo",
  expectedEntryId: string,
  history: { past: MockBoardHistoryEntry[]; future: MockBoardHistoryEntry[] } | undefined,
): never {
  const undoEntryId = mockBoardEntryId(history, "past");
  const redoEntryId = mockBoardEntryId(history, "future");
  const currentEntryId = action === "undo" ? undoEntryId : redoEntryId;
  throw {
    code: "conflict",
    message: `board ${action} for project \`${projectId}\` expected history entry \`${expectedEntryId}\`, but the current ${action} entry is ${currentEntryId === null ? "none" : `\`${currentEntryId}\``}`,
    details: {
      project_id: projectId,
      action,
      expected_entry_id: expectedEntryId,
      undo_entry_id: undoEntryId,
      redo_entry_id: redoEntryId,
    },
  };
}

export function mockResetBoard(projectId?: string): void {
  if (projectId) {
    delete mockBoards[projectId];
    delete mockBoardHistory[projectId];
    delete mockActivity[projectId];
  } else {
    for (const key of Object.keys(mockBoards)) {
      delete mockBoards[key];
    }
    for (const key of Object.keys(mockBoardHistory)) {
      delete mockBoardHistory[key];
    }
    for (const key of Object.keys(mockActivity)) {
      delete mockActivity[key];
    }
  }
}

/* ───── Dev activity feed (activity.list, activity.appended) ─────
   Fixture rows make the browser preview show the panel. Live board edits
   append a record and emit the same event the host sends, so the feed, the
   undo stack and the canvas trace behave as they do in Tauri. */

const mockActivity: Record<string, ActivityRecord[]> = {};
let mockActivitySequence = 0;

function mockDemoActivity(projectId: string): ActivityRecord[] {
  const now = Date.now();
  const at = (minutesAgo: number) => new Date(now - minutesAgo * 60_000).toISOString();
  return [
    {
      id: `act_demo_agent_${projectId}`,
      at: at(2),
      actor: { kind: "Agent", id: "agent-demo", label: "Agent" },
      summary: "Applied 2 board changes",
      summaryKey: "activity.board.apply",
      summaryParams: { count: 2 },
      targets: ["note-demo-2", "note-demo-3"],
      kind: "applied",
      historyEntryId: "board-history:demo-agent",
    },
    {
      id: `act_demo_note_${projectId}`,
      at: at(5),
      actor: { kind: "Human", id: null, label: null },
      summary: "Added a note",
      summaryKey: "activity.board.addNote",
      summaryParams: { count: 1 },
      targets: ["note-demo-1"],
      kind: "applied",
      historyEntryId: "board-history:demo-note",
    },
    {
      id: `act_demo_op_${projectId}`,
      at: at(12),
      actor: { kind: "Human", id: null, label: null },
      summary: "Ran collection.create",
      summaryKey: "activity.op",
      summaryParams: { op: "collection.create" },
      targets: [],
      kind: "applied",
      historyEntryId: null,
    },
    {
      id: `act_demo_undo_${projectId}`,
      at: at(20),
      actor: { kind: "Human", id: null, label: null },
      summary: "Undid the last board change",
      summaryKey: "activity.board.undo",
      summaryParams: {},
      targets: ["note-demo-1"],
      kind: "undone",
      historyEntryId: "board-history:demo-note",
    },
  ];
}

/** Activity for one project, seeded newest first on first use. */
function mockActivityFor(projectId: string): ActivityRecord[] {
  let records = mockActivity[projectId];
  if (!records) {
    records = projectId === MOCK_FIXTURE_PROJECT_ID ? mockDemoActivity(projectId) : [];
    mockActivity[projectId] = records;
  }
  return records;
}

function appendMockActivity(
  projectId: string,
  record: Omit<ActivityRecord, "id" | "at">,
): ActivityRecord {
  mockActivitySequence += 1;
  const appended: ActivityRecord = {
    ...record,
    id: `act_mock_${Date.now().toString(36)}_${mockActivitySequence}`,
    at: new Date().toISOString(),
  };
  mockActivityFor(projectId).unshift(appended);
  emitEvent({
    type: "mod",
    kind: "activity.appended",
    payload: { projectId, record: appended },
  });
  return appended;
}

/** Summary key and English fallback for an applied mock batch (host mirror). */
function mockApplySummary(commands: RawBoardCommand[]): {
  summaryKey: string;
  summary: string;
} {
  const count = commands.length;
  const allAdds = commands.every((command) => command.type === "add_item");
  const allNotes =
    allAdds &&
    commands.every((command) => command.type === "add_item" && command.kind.type === "note");

  if (allNotes) {
    return {
      summaryKey: "activity.board.addNote",
      summary: count === 1 ? "Added a note" : `Added ${count} notes`,
    };
  }
  if (allAdds) {
    return {
      summaryKey: "activity.board.addItem",
      summary: count === 1 ? "Added an item" : `Added ${count} items`,
    };
  }
  return {
    summaryKey: "activity.board.apply",
    summary: count === 1 ? "Applied a board change" : `Applied ${count} board changes`,
  };
}

export async function mockGetBoardSnapshot(input: { projectId: string }): Promise<BoardSnapshot> {
  if (!mockBoards[input.projectId]) {
    mockBoards[input.projectId] = {
      id: "board-" + input.projectId,
      projectId: input.projectId,
      version: 0,
      items: [],
    };
    mockBoardHistory[input.projectId] = { past: [], future: [] };
  }
  return mockBoards[input.projectId];
}

type RawBoardCommand =
  | {
      type: "add_item";
      item_id: string;
      kind: BoardItem["kind"];
      position: { x: number; y: number };
      size: { width: number; height: number };
    }
  | { type: "remove_item"; item_id: string }
  | { type: "move_item"; item_id: string; position: { x: number; y: number } }
  | { type: "resize_item"; item_id: string; size: { width: number; height: number } }
  | { type: "set_z"; item_id: string; z: number }
  | { type: "update_note"; item_id: string; content: string }
  | { type: "lock_item"; item_id: string; locked: boolean };

function itemMissingDiagnostic(itemId: string) {
  return {
    id: `board-item-missing-${itemId}`,
    code: "CORE/BOARD_ITEM_MISSING",
    severity: "error",
    source: "core",
    message: "board item does not exist",
    target: "board",
  };
}

function itemLockedDiagnostic(itemId: string) {
  return {
    id: `board-item-locked-${itemId}`,
    code: "CORE/BOARD_ITEM_LOCKED",
    severity: "error",
    source: "core",
    message: "cannot modify locked board item",
    target: "board",
  };
}

function evaluateBoardCommands(
  currentItems: BoardItem[],
  commands: RawBoardCommand[],
): { nextItems: BoardItem[]; changes: unknown[]; diagnostics: unknown[] } {
  const nextItems: BoardItem[] = JSON.parse(JSON.stringify(currentItems));
  const diagnostics: unknown[] = [];
  const changes: unknown[] = [];

  for (const cmd of commands) {
    if (!cmd || typeof cmd !== "object") continue;
    switch (cmd.type) {
      case "add_item": {
        const { item_id, kind, position, size } = cmd;
        if (nextItems.some((i) => i.id === item_id)) {
          diagnostics.push({
            id: `board-item-duplicate-${item_id}`,
            code: "CORE/BOARD_ITEM_DUPLICATE",
            severity: "error",
            source: "core",
            message: "board item id already exists",
            target: "board",
          });
          break;
        }
        nextItems.push({
          id: item_id,
          kind,
          position: { x: position?.x ?? 0, y: position?.y ?? 0 },
          size: { width: size?.width ?? 0, height: size?.height ?? 0 },
          z: 0,
          locked: false,
        });
        changes.push({ type: "item_added", item_id });
        break;
      }
      case "remove_item": {
        const { item_id } = cmd;
        const idx = nextItems.findIndex((i) => i.id === item_id);
        if (idx === -1) {
          diagnostics.push(itemMissingDiagnostic(item_id));
          break;
        }
        nextItems.splice(idx, 1);
        changes.push({ type: "item_removed", item_id });
        break;
      }
      case "move_item": {
        const { item_id, position } = cmd;
        const item = nextItems.find((i) => i.id === item_id);
        if (!item) {
          diagnostics.push(itemMissingDiagnostic(item_id));
          break;
        }
        if (item.locked) {
          diagnostics.push(itemLockedDiagnostic(item_id));
          break;
        }
        if (item.position.x === position.x && item.position.y === position.y) {
          break;
        }
        item.position = { x: position.x, y: position.y };
        changes.push({ type: "item_moved", item_id, position });
        break;
      }
      case "resize_item": {
        const { item_id, size } = cmd;
        const item = nextItems.find((i) => i.id === item_id);
        if (!item) {
          diagnostics.push(itemMissingDiagnostic(item_id));
          break;
        }
        if (item.locked) {
          diagnostics.push(itemLockedDiagnostic(item_id));
          break;
        }
        if (item.size.width === size.width && item.size.height === size.height) {
          break;
        }
        item.size = { width: size.width, height: size.height };
        changes.push({ type: "item_resized", item_id, size });
        break;
      }
      case "set_z": {
        const { item_id, z } = cmd;
        const item = nextItems.find((i) => i.id === item_id);
        if (!item) {
          diagnostics.push(itemMissingDiagnostic(item_id));
          break;
        }
        if (item.z === z) {
          break;
        }
        item.z = z;
        changes.push({ type: "item_z_changed", item_id, z });
        break;
      }
      case "update_note": {
        const { item_id, content } = cmd;
        const item = nextItems.find((i) => i.id === item_id);
        if (!item) {
          diagnostics.push(itemMissingDiagnostic(item_id));
          break;
        }
        if (item.locked) {
          diagnostics.push(itemLockedDiagnostic(item_id));
          break;
        }
        if (item.kind.type !== "note") {
          diagnostics.push({
            id: `board-item-not-note-${item_id}`,
            code: "CORE/BOARD_ITEM_NOT_NOTE",
            severity: "error",
            source: "core",
            message: "board item is not a note",
            target: "board",
          });
          break;
        }
        if ((item.kind as { type: "note"; content: string }).content === content) {
          break;
        }
        (item.kind as { type: "note"; content: string }).content = content;
        changes.push({ type: "note_updated", item_id, content });
        break;
      }
      case "lock_item": {
        const { item_id, locked } = cmd;
        const item = nextItems.find((i) => i.id === item_id);
        if (!item) {
          diagnostics.push(itemMissingDiagnostic(item_id));
          break;
        }
        if (item.locked === locked) {
          break;
        }
        item.locked = locked;
        changes.push({ type: "item_locked", item_id, locked });
        break;
      }
    }
  }

  return { nextItems, changes, diagnostics };
}

export async function mockPreviewBoardCommands(input: {
  projectId: string;
  commandBatch: unknown;
}): Promise<BoardCommandResultDto> {
  const board = await mockGetBoardSnapshot({ projectId: input.projectId });
  const batch = input.commandBatch as
    | {
        base_version?: number;
        commands?: RawBoardCommand[];
      }
    | null
    | undefined;

  if (batch?.base_version !== undefined && batch.base_version !== board.version) {
    return {
      status: "rejected",
      version: board.version,
      changes: [],
      diagnostics: [
        {
          id: `board-version-conflict-${board.id}-${batch.base_version}`,
          code: "CORE/BOARD_VERSION_CONFLICT",
          severity: "error",
          source: "core",
          message: `board version conflict: expected ${board.version}, got ${batch.base_version}`,
          target: "board",
        },
      ],
      historyEntryId: null,
    };
  }

  const commands = batch?.commands ?? [];
  if (commands.length === 0) {
    return {
      status: "no_op",
      version: board.version,
      changes: [],
      diagnostics: [],
      historyEntryId: null,
    };
  }

  const { changes, diagnostics } = evaluateBoardCommands(board.items, commands);
  if (diagnostics.length > 0) {
    return {
      status: "rejected",
      version: board.version,
      changes: [],
      diagnostics,
      historyEntryId: null,
    };
  }

  if (changes.length === 0) {
    return {
      status: "no_op",
      version: board.version,
      changes: [],
      diagnostics: [],
      historyEntryId: null,
    };
  }

  return {
    status: "applied",
    version: board.version + 1,
    changes,
    diagnostics: [],
    historyEntryId: null,
  };
}

export async function mockApplyBoardCommands(input: {
  projectId: string;
  commandBatch: unknown;
}): Promise<BoardCommandResultDto> {
  const board = await mockGetBoardSnapshot({ projectId: input.projectId });
  const history =
    mockBoardHistory[input.projectId] ??
    (mockBoardHistory[input.projectId] = { past: [], future: [] });

  const batch = input.commandBatch as
    | {
        id?: string;
        base_version?: number;
        commands?: RawBoardCommand[];
      }
    | null
    | undefined;

  if (batch?.base_version !== undefined && batch.base_version !== board.version) {
    return {
      status: "rejected",
      version: board.version,
      changes: [],
      diagnostics: [
        {
          id: `board-version-conflict-${board.id}-${batch.base_version}`,
          code: "CORE/BOARD_VERSION_CONFLICT",
          severity: "error",
          source: "core",
          message: `board version conflict: expected ${board.version}, got ${batch.base_version}`,
          target: "board",
        },
      ],
      historyEntryId: null,
    };
  }

  const commands = batch?.commands ?? [];
  if (commands.length === 0) {
    return {
      status: "no_op",
      version: board.version,
      changes: [],
      diagnostics: [],
      historyEntryId: null,
    };
  }

  const { nextItems, changes, diagnostics } = evaluateBoardCommands(board.items, commands);
  if (diagnostics.length > 0) {
    return {
      status: "rejected",
      version: board.version,
      changes: [],
      diagnostics,
      historyEntryId: null,
    };
  }

  if (changes.length === 0) {
    return {
      status: "no_op",
      version: board.version,
      changes: [],
      diagnostics: [],
      historyEntryId: null,
    };
  }

  const entryId = `board-history:${batch?.id ?? rand("batch")}`;
  // Mirrors the host: one target per changed item, in change order.
  const targets = [
    ...new Set(
      changes
        .map((change) => (change as { item_id?: string }).item_id)
        .filter((id): id is string => typeof id === "string"),
    ),
  ];
  history.past.push({
    items: JSON.parse(JSON.stringify(board.items)),
    entryId,
    targets,
  });
  history.future = [];
  board.items = nextItems;
  board.version += 1;

  emitEvent({
    type: "document",
    kind: "board.changed",
    projectId: input.projectId,
    documentId: board.id,
    version: board.version,
  });

  const { summaryKey, summary } = mockApplySummary(commands);
  appendMockActivity(input.projectId, {
    actor: { kind: "Human", id: null, label: null },
    summary,
    summaryKey,
    summaryParams: { count: commands.length },
    targets,
    kind: "applied",
    historyEntryId: entryId,
  });

  return {
    status: "applied",
    version: board.version,
    changes,
    diagnostics: [],
    historyEntryId: entryId,
  };
}

export async function mockUndoBoard(input: {
  projectId: string;
  expectedEntryId?: string | null;
}): Promise<BoardCommandResultDto | null> {
  const board = await mockGetBoardSnapshot({ projectId: input.projectId });
  const history = mockBoardHistory[input.projectId];
  const expected = input.expectedEntryId;
  if (expected != null && mockBoardEntryId(history, "past") !== expected) {
    rejectBoardHistoryMismatch(input.projectId, "undo", expected, history);
  }
  if (!history || history.past.length === 0) {
    return null;
  }
  const entry = history.past.pop()!;
  history.future.push({
    items: JSON.parse(JSON.stringify(board.items)),
    entryId: entry.entryId,
    targets: entry.targets,
  });
  board.items = entry.items;
  board.version += 1;

  emitEvent({
    type: "document",
    kind: "board.changed",
    projectId: input.projectId,
    documentId: board.id,
    version: board.version,
  });

  appendMockActivity(input.projectId, {
    actor: { kind: "Human", id: null, label: null },
    summary: "Undid the last board change",
    summaryKey: "activity.board.undo",
    summaryParams: {},
    targets: entry.targets,
    kind: "undone",
    historyEntryId: entry.entryId,
  });

  return {
    status: "applied",
    version: board.version,
    changes: [],
    diagnostics: [],
    historyEntryId: entry.entryId,
  };
}

export async function mockRedoBoard(input: {
  projectId: string;
  expectedEntryId?: string | null;
}): Promise<BoardCommandResultDto | null> {
  const board = await mockGetBoardSnapshot({ projectId: input.projectId });
  const history = mockBoardHistory[input.projectId];
  const expected = input.expectedEntryId;
  if (expected != null && mockBoardEntryId(history, "future") !== expected) {
    rejectBoardHistoryMismatch(input.projectId, "redo", expected, history);
  }
  if (!history || history.future.length === 0) {
    return null;
  }
  const entry = history.future.pop()!;
  history.past.push({
    items: JSON.parse(JSON.stringify(board.items)),
    entryId: entry.entryId,
    targets: entry.targets,
  });
  board.items = entry.items;
  board.version += 1;

  emitEvent({
    type: "document",
    kind: "board.changed",
    projectId: input.projectId,
    documentId: board.id,
    version: board.version,
  });

  appendMockActivity(input.projectId, {
    actor: { kind: "Human", id: null, label: null },
    summary: "Redid the last board change",
    summaryKey: "activity.board.redo",
    summaryParams: {},
    targets: entry.targets,
    kind: "redone",
    historyEntryId: entry.entryId,
  });

  return {
    status: "applied",
    version: board.version,
    changes: [],
    diagnostics: [],
    historyEntryId: entry.entryId,
  };
}

export async function mockListModels(): Promise<ModelInfo[]> {
  await delay(150);
  return [...MOCK_MODELS];
}

const MOCK_CATALOG: ModelCatalogEntry[] = [
  {
    id: "stabilityai/stable-diffusion-xl-base-1.0",
    author: "stabilityai",
    pipelineTag: "text-to-image",
    tags: ["diffusers", "safetensors"],
    downloads: 12_400_000,
    likes: 10_200,
    lastModified: "2024-01-15T00:00:00Z",
    private: false,
  },
  {
    id: "runwayml/stable-diffusion-v1-5",
    author: "runwayml",
    pipelineTag: "text-to-image",
    tags: ["diffusers", "safetensors"],
    downloads: 25_100_000,
    likes: 21_300,
    lastModified: "2023-10-01T00:00:00Z",
    private: false,
  },
];

const MOCK_CARD: ModelCard = {
  entry: MOCK_CATALOG[0],
  detectedFormat: "Diffusers",
  estimatedDownloadSize: 6_940_000_000,
  modelSummary:
    "Stable Diffusion XL base is a latent diffusion model for text-to-image generation.",
  fileCount: 12,
  components: ["unet", "text_encoder", "text_encoder_2", "vae"],
};

export async function mockSearchModels(input: {
  query: string;
  filters?: unknown;
}): Promise<ModelCatalogEntry[]> {
  await delay(200);
  const needle = input.query.trim().toLowerCase();
  if (!needle) return [];
  return MOCK_CATALOG.filter((entry) => entry.id.toLowerCase().includes(needle));
}

export async function mockGetModelCard(_input: { repoId: string }): Promise<ModelCard> {
  await delay(150);
  return MOCK_CARD;
}

export async function mockDownloadHuggingfaceModel(
  args: DownloadHuggingfaceModelArgs,
): Promise<ModelDownloadOutput> {
  const total = 6_940_000_000;
  const chunk = 500_000_000;
  let bytes = 0;
  const repoId = args.repoId;

  const emit = (stage: string, message: string | null = null) => {
    const payload: DownloadProgress = {
      downloadId: args.downloadId,
      bytesDownloaded: bytes,
      totalBytes: total,
      message,
    };
    emitEvent({ type: "mod", kind: `aigc.download.${stage}`, payload });
  };

  emit("started");
  while (bytes < total) {
    await delay(80);
    bytes = Math.min(total, bytes + chunk);
    emit("progress", "Downloading model files");
  }
  emit("completed");

  return {
    effective: true,
    provider: "hf",
    repoId,
    revision: args.revision ?? "main",
    targetDir: args.targetRelativeDir,
    files: [{ relativePath: "model_index.json", bytes: total, outcome: "downloaded" }],
    totalBytes: total,
    finishedAt: new Date().toISOString(),
    detectedFormat: "Diffusers",
  };
}

/* ───── Operation Orchestration Mocks (W-a / W-b) ───── */

export type MockCollectionDoc = {
  id: string;
  projectId: string;
  name: string;
  version: number;
  slots: Array<{ id: string; asset_id: string; stale?: boolean }>;
};

type MockRecipeStep = {
  step_id: string;
  op: string;
  op_version: string;
  input: Record<string, unknown>;
  input_sources: Record<string, { step_id: string; output_pointer: string }>;
  output: Record<string, unknown> | null;
  actor: {
    kind: "Human" | "Agent" | "Importer" | "System";
    id: string | null;
    label: string | null;
  };
  read_only: boolean;
  stale: boolean;
};

type MockSlotRecipe = {
  collection_id: string;
  slot_id: string;
  root_asset_id: string;
  steps: MockRecipeStep[];
};

type MockAsset = {
  projectId: string;
  mediaType: string;
  content: string;
  libraryItem?: string;
};

export type MockLibraryItem = {
  id: string;
  name: string;
  tags: string[];
  media_type: string;
  size: number;
  hash: string;
  added_at: string;
  removed: boolean;
  source: unknown;
  content?: string;
};

let mockLibraryVersion = 1;
export const mockLibraryItems: MockLibraryItem[] = [
  {
    id: "lib_item_1",
    name: "design_system_preview.png",
    tags: ["ui", "design"],
    media_type: "image/png",
    size: 245_760,
    hash: "h_img_123",
    added_at: "2026-10-01T10:00:00Z",
    removed: false,
    source: { type: "local", path: "/assets/design_system_preview.png" },
    content: "mock binary png data",
  },
  {
    id: "lib_item_2",
    name: "prompts_master.txt",
    tags: ["prompt", "llm"],
    media_type: "text/plain",
    size: 1_280,
    hash: "h_txt_456",
    added_at: "2026-10-02T12:30:00Z",
    removed: false,
    source: { type: "local", path: "/assets/prompts_master.txt" },
    content:
      "Master prompt definitions:\n1. Creative generation\n2. Style transfer\n3. Consistent characters",
  },
  {
    id: "lib_item_3",
    name: "workflow_spec.json",
    tags: ["workflow"],
    media_type: "application/json",
    size: 3_420,
    hash: "h_json_789",
    added_at: "2026-10-03T09:15:00Z",
    removed: false,
    source: { type: "local", path: "/assets/workflow_spec.json" },
    content: '{\n  "version": 1,\n  "name": "Multi-pass refinement",\n  "steps": 3\n}',
  },
];

const mockLibraryHistory: {
  past: MockLibraryItem[][];
  future: MockLibraryItem[][];
} = { past: [], future: [] };

const mockWorkspaceFolders: Record<string, { id: string; path: string }[]> = {
  default: [{ id: "fld_1", path: "/Users/pref2rence/project/Reimagine/sample_workspace" }],
};

/** Project that owns the dev fixture library, mirroring the seeded Default project. */
const MOCK_FIXTURE_PROJECT_ID = "default";

/** Dev fixture collection so the preview library has a collection row. */
const mockCollections: Record<string, MockCollectionDoc> = {
  col_demo_drafts: {
    id: "col_demo_drafts",
    projectId: MOCK_FIXTURE_PROJECT_ID,
    name: "Blog drafts",
    version: 4,
    slots: [
      { id: "slot-1", asset_id: "ast_demo_style_guide" },
      { id: "slot-2", asset_id: "ast_demo_blog_v2" },
      { id: "slot-3", asset_id: "ast_demo_cover_brief" },
    ],
  },
};
type MockCollectionSnapshot = {
  doc: MockCollectionDoc;
  recipes: Record<string, MockSlotRecipe>;
};

const mockCollectionHistory: Record<
  string,
  { past: MockCollectionSnapshot[]; future: MockCollectionSnapshot[] }
> = {};
const mockSlotRecipes: Record<string, MockSlotRecipe> = {};
const mockAssetProvenance: Record<
  string,
  {
    op: string;
    op_version: string;
    input: Record<string, unknown>;
    output: Record<string, unknown>;
    read_only: boolean;
  }
> = {};

function recipeKey(collectionId: string, slotId: string): string {
  return `${collectionId}:${slotId}`;
}

function snapshotCollectionRecipes(collectionId: string): Record<string, MockSlotRecipe> {
  const result: Record<string, MockSlotRecipe> = {};
  const prefix = `${collectionId}:`;
  for (const [key, recipe] of Object.entries(mockSlotRecipes)) {
    if (key.startsWith(prefix)) {
      result[key] = JSON.parse(JSON.stringify(recipe)) as MockSlotRecipe;
    }
  }
  return result;
}

function restoreCollectionRecipes(
  collectionId: string,
  recipes: Record<string, MockSlotRecipe>,
): void {
  const prefix = `${collectionId}:`;
  for (const key of Object.keys(mockSlotRecipes)) {
    if (key.startsWith(prefix)) {
      delete mockSlotRecipes[key];
    }
  }
  for (const [key, recipe] of Object.entries(recipes)) {
    mockSlotRecipes[key] = JSON.parse(JSON.stringify(recipe)) as MockSlotRecipe;
  }
}
/** Dev fixture files so the preview library has file rows. */
const mockAssets: Record<string, MockAsset> = {
  ast_demo_style_guide: {
    projectId: MOCK_FIXTURE_PROJECT_ID,
    mediaType: "text/markdown",
    content:
      "# Style guide\n\nVoice, headings and image ratios for the workspace.\n\n" +
      "Use sentence case for headings and plain verbs for actions.\n".repeat(40),
  },
  ast_demo_blog_v2: {
    projectId: MOCK_FIXTURE_PROJECT_ID,
    mediaType: "text/markdown",
    content:
      "# Blog v2\n\nOutline and opening paragraph.\n\n" +
      "The draft keeps one idea per section and cites its source.\n".repeat(28),
  },
  ast_demo_cover_brief: {
    projectId: MOCK_FIXTURE_PROJECT_ID,
    mediaType: "text/plain",
    content:
      "Cover brief: warm grey background, one image, no headline.\n" +
      "Keep the lower third clear for the series label.\n".repeat(55),
  },
};

const MOCK_DYNAMIC_WORKFLOWS = [
  {
    id: "dwf_demo_cleanup",
    projectId: MOCK_FIXTURE_PROJECT_ID,
    name: "Draft cleanup",
    version: 3,
    step_count: 3,
  },
  {
    id: "dwf_demo_archive",
    projectId: MOCK_FIXTURE_PROJECT_ID,
    name: "Archive sweep",
    version: 1,
    step_count: 2,
  },
];

const MOCK_DEMO_RUN_ID = "run_2";

function mockDemoRun(projectId: string) {
  const now = Date.now();
  return {
    id: MOCK_DEMO_RUN_ID,
    project_id: projectId,
    dynamic_workflow_id: "dwf_demo_cleanup",
    target: { collection_id: "col_demo_drafts" },
    params: {},
    state: "completed",
    version: 1,
    items: [
      { item_id: "slot-1", slot_id: "slot-1", state: "succeeded" },
      { item_id: "slot-2", slot_id: "slot-2", state: "succeeded" },
      { item_id: "slot-3", slot_id: "slot-3", state: "succeeded" },
    ],
    collection_id: "col_demo_drafts",
    created_at: new Date(now - 6 * 60_000).toISOString(),
    finished_at: new Date(now - 4 * 60_000).toISOString(),
  };
}

export async function mockReadAssetPreview(
  url: string,
  assetId: string,
  maxLines: number,
): Promise<{ url: string; mediaType: string; text: string | null }> {
  const asset = mockAssets[assetId];
  if (asset) {
    const textual = asset.mediaType.startsWith("text/") || asset.mediaType === "application/json";
    return {
      url,
      mediaType: asset.mediaType,
      text: textual ? asset.content.split("\n").slice(0, maxLines).join("\n") : null,
    };
  }
  const libItem = mockLibraryItems.find((item) => item.id === assetId);
  if (libItem) {
    const textual =
      libItem.media_type.startsWith("text/") || libItem.media_type === "application/json";
    return {
      url,
      mediaType: libItem.media_type,
      text: textual ? (libItem.content ?? "").split("\n").slice(0, maxLines).join("\n") : null,
    };
  }
  throw new Error(`mock asset ${assetId} not found`);
}

export async function mockReadAssetText(projectId: string, assetId: string): Promise<string> {
  void projectId;
  const asset = mockAssets[assetId];
  if (asset) return asset.content;
  const libItem = mockLibraryItems.find((item) => item.id === assetId);
  if (libItem && libItem.content) return libItem.content;
  throw new Error(`mock asset ${assetId} not found`);
}

/* ───── `op_list` descriptors and `op_preview` ───── */

function textDocumentProperties(): Record<string, unknown> {
  return {
    doc: {
      type: "string",
      format: "asset",
      contentMediaType: "text/*",
      description: "An asset path. Give exactly one of `doc` and `text`.",
    },
    text: {
      type: "string",
      format: "inline-text",
      description: "Inline text (at most 256 KiB). Give exactly one of `doc` and `text`.",
    },
  };
}

function textOperation(
  name: string,
  description: string,
  properties: Record<string, unknown>,
  required: string[],
  annotations: Record<string, unknown>,
  overrides?: {
    input_schema?: Record<string, unknown>;
    output_schema?: Record<string, unknown>;
  },
): OperationInfo {
  return {
    spec: {
      name,
      description,
      input_schema: overrides?.input_schema ?? {
        type: "object",
        properties: { ...textDocumentProperties(), ...properties },
        required,
        additionalProperties: false,
      },
      output_schema: overrides?.output_schema ?? {
        type: "object",
        properties: textDocumentProperties(),
        required: [],
        additionalProperties: false,
      },
      annotations,
    },
    provider: { mod: { id: "text" } },
    health: { status: "available" },
  };
}

const TEXT_ANNOTATIONS = {
  read_only: false,
  destructive: false,
  idempotent: false,
  deterministic: true,
  open_world: false,
};

const MOCK_TEXT_OPERATIONS: OperationInfo[] = [
  textOperation(
    "text.normalize",
    "Normalize a document's whitespace, full-width punctuation, line endings and Unicode form.",
    {
      whitespace: { type: "boolean", default: true },
      punctuation: { type: "string", enum: ["none", "halfwidth"], default: "none" },
      newline: { type: "boolean", default: true },
      unicode: { type: "string", enum: ["NFC", "none"], default: "NFC" },
    },
    [],
    { ...TEXT_ANNOTATIONS, idempotent: true },
  ),
  textOperation(
    "text.replace",
    "Replace literal strings, whole words or regular expressions and report the count per pair.",
    {
      pairs: {
        type: "array",
        description: "Replacement pairs, applied in order.",
        items: {
          type: "object",
          properties: {
            find: { type: "string", minLength: 1 },
            replace: {
              type: "string",
              description:
                "In `regex` mode `$1` refers to a capture group; in the other modes the text is literal.",
            },
            mode: { type: "string", enum: ["literal", "word", "regex"] },
            case_sensitive: { type: "boolean", default: true },
          },
          required: ["find", "replace", "mode"],
          additionalProperties: false,
        },
      },
    },
    ["pairs"],
    TEXT_ANNOTATIONS,
  ),
  textOperation(
    "text.inspect",
    "Read a document's title, ATX headings, flat front matter and text statistics as JSON.",
    {},
    [],
    { ...TEXT_ANNOTATIONS, read_only: true },
    {
      output_schema: {
        type: "object",
        properties: {
          title: { type: ["string", "null"] },
          headings: { type: "array", items: { type: "object" } },
          front_matter: { type: "object" },
          stats: { type: "object" },
        },
        required: ["headings", "front_matter", "stats"],
        additionalProperties: false,
      },
    },
  ),
  textOperation(
    "text.template",
    "Render a `{{name}}` template from values, the document's front matter and its body.",
    {
      template: { type: "string" },
      values: { type: "object", default: {} },
    },
    ["template"],
    TEXT_ANNOTATIONS,
  ),
  textOperation(
    "text.split",
    "Split a document at a heading level or a literal separator; every piece carries a title.",
    {
      by: {
        type: "object",
        properties: {
          kind: { type: "string", enum: ["heading", "separator"] },
          level: { type: "integer", minimum: 1, maximum: 6 },
          separator: { type: "string", minLength: 1 },
        },
        required: ["kind"],
        additionalProperties: false,
      },
    },
    ["by"],
    TEXT_ANNOTATIONS,
    {
      output_schema: {
        type: "object",
        properties: {
          docs: {
            type: "array",
            items: {
              type: "object",
              properties: {
                title: { type: "string" },
                ...textDocumentProperties(),
              },
            },
          },
        },
        required: ["docs"],
        additionalProperties: false,
      },
    },
  ),
  textOperation(
    "text.join",
    "Join documents in order with a separator, or render a template for each and concatenate.",
    {},
    [],
    TEXT_ANNOTATIONS,
    {
      input_schema: {
        type: "object",
        properties: {
          docs: {
            type: "array",
            items: { type: "string", format: "asset", contentMediaType: "text/*" },
          },
          texts: { type: "array", items: { type: "string" } },
          separator: { type: "string" },
          template: { type: "string" },
        },
        additionalProperties: false,
      },
    },
  ),
];

export async function mockOpList(input: { projectId: string }): Promise<OperationInfo[]> {
  void input.projectId;
  return MOCK_TEXT_OPERATIONS.map((operation) => ({
    ...operation,
    spec: { ...operation.spec },
  }));
}

type MockPair = {
  find?: unknown;
  replace?: unknown;
  mode?: unknown;
  case_sensitive?: unknown;
};

/** Apply replace pairs the way the text mod does, including per-pair counts. */
function applyMockReplacements(
  text: string,
  pairs: MockPair[],
): { text: string; counts: number[] } {
  let result = text;
  const counts: number[] = [];
  for (const pair of pairs) {
    const find = typeof pair.find === "string" ? pair.find : "";
    const replacement = typeof pair.replace === "string" ? pair.replace : "";
    if (find.length === 0) {
      counts.push(0);
      continue;
    }
    const mode = typeof pair.mode === "string" ? pair.mode : "literal";
    const caseSensitive = pair.case_sensitive !== false;
    const flags = caseSensitive ? "g" : "gi";
    let pattern: RegExp;
    if (mode === "regex") {
      pattern = new RegExp(find, flags);
      const matches = result.match(pattern)?.length ?? 0;
      result = result.replace(pattern, replacement);
      counts.push(matches);
      continue;
    }
    const escaped = find.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    pattern = new RegExp(mode === "word" ? `\\b${escaped}\\b` : escaped, flags);
    const matches = result.match(pattern)?.length ?? 0;
    // Literal and word replacements never expand `$1` groups.
    result = result.replace(pattern, () => replacement);
    counts.push(matches);
  }
  return { text: result, counts };
}

function mockDocumentText(projectId: string, input: Record<string, unknown>): string {
  if (typeof input.text === "string") return input.text;
  const docId = typeof input.doc === "string" ? input.doc : "";
  const asset = mockAssets[docId];
  if (!asset) throw new Error(`mock asset ${docId || "(missing doc)"} not found`);
  void projectId;
  return asset.content;
}

const FULLWIDTH_PUNCTUATION: Record<string, string> = {
  "，": ",",
  "。": ".",
  "！": "!",
  "？": "?",
  "；": ";",
  "：": ":",
  "（": "(",
  "）": ")",
  "【": "[",
  "】": "]",
  "“": '"',
  "”": '"',
  "‘": "'",
  "’": "'",
  "、": ",",
  "\u3000": " ",
};

function applyMockNormalize(text: string, args: Record<string, unknown>): string {
  let result = text;
  const unicode = typeof args.unicode === "string" ? args.unicode : "NFC";
  if (unicode === "NFC") {
    result = result.normalize("NFC");
  }
  const newline = args.newline !== false;
  if (newline) {
    result = result.replace(/\r\n?/g, "\n");
  }
  const punctuation = typeof args.punctuation === "string" ? args.punctuation : "none";
  if (punctuation === "halfwidth") {
    result = result.replace(/[\uFF01-\uFF5E\u3000，。！？；：（）【】“”‘’、]/g, (ch) => {
      if (ch in FULLWIDTH_PUNCTUATION) return FULLWIDTH_PUNCTUATION[ch];
      const code = ch.charCodeAt(0);
      if (code >= 0xff01 && code <= 0xff5e) {
        return String.fromCharCode(code - 0xfee0);
      }
      return ch;
    });
  }
  const whitespace = args.whitespace !== false;
  if (whitespace) {
    result = result
      .split("\n")
      .map((line) => line.replace(/[ \t]+/g, " ").trimEnd())
      .join("\n");
  }
  return result;
}

function parseFrontMatter(text: string): { frontMatter: Record<string, string>; body: string } {
  const normalized = text.replace(/\r\n?/g, "\n");
  if (!normalized.startsWith("---\n")) {
    return { frontMatter: {}, body: text };
  }
  const endIdx = normalized.indexOf("\n---\n", 4);
  if (endIdx === -1) {
    return { frontMatter: {}, body: text };
  }
  const header = normalized.slice(4, endIdx);
  const body = normalized.slice(endIdx + 5);
  const frontMatter: Record<string, string> = {};
  for (const line of header.split("\n")) {
    const colon = line.indexOf(":");
    if (colon <= 0) continue;
    const key = line.slice(0, colon).trim();
    const val = line.slice(colon + 1).trim();
    if (key) frontMatter[key] = val;
  }
  return { frontMatter, body };
}

function applyMockTemplate(text: string, args: Record<string, unknown>): string {
  const template = typeof args.template === "string" ? args.template : "{{body}}";
  const rawValues =
    args.values && typeof args.values === "object" && !Array.isArray(args.values)
      ? (args.values as Record<string, unknown>)
      : {};
  const { frontMatter, body } = parseFrontMatter(text);
  const vars: Record<string, string> = { ...frontMatter, body };
  for (const [k, v] of Object.entries(rawValues)) {
    vars[k] = v == null ? "" : String(v);
  }
  return template.replace(/\{\{\s*([a-zA-Z0-9_.-]+)\s*\}\}/g, (_match, key: string) => {
    return key in vars ? vars[key] : "";
  });
}

function applyMockInspect(text: string): Record<string, unknown> {
  const { frontMatter, body } = parseFrontMatter(text);
  const lines = body.split(/\r?\n/);
  const headings: Array<{ level: number; text: string }> = [];
  let title: string | null = null;
  for (const line of lines) {
    const match = /^(#{1,6})\s+(.+)$/.exec(line.trim());
    if (!match) continue;
    const level = match[1].length;
    const headingText = match[2].trim();
    headings.push({ level, text: headingText });
    if (title === null && level === 1) {
      title = headingText;
    }
  }
  const words = body.trim() ? body.trim().split(/\s+/).length : 0;
  return {
    title,
    headings,
    front_matter: frontMatter,
    stats: {
      chars: body.length,
      words,
      lines: lines.length,
      bytes: new TextEncoder().encode(text).length,
    },
  };
}

export async function mockOpPreview(input: {
  projectId: string;
  name: string;
  input: unknown;
  previewKey: string;
}): Promise<unknown> {
  const args = (input.input as Record<string, unknown>) ?? {};
  if (input.name === "text.replace") {
    const pairs = Array.isArray(args.pairs) ? (args.pairs as MockPair[]) : [];
    const original = mockDocumentText(input.projectId, args);
    return applyMockReplacements(original, pairs);
  }
  if (input.name === "text.normalize") {
    const original = mockDocumentText(input.projectId, args);
    return { text: applyMockNormalize(original, args) };
  }
  if (input.name === "text.template") {
    const original = mockDocumentText(input.projectId, args);
    return { text: applyMockTemplate(original, args) };
  }
  throw new Error(`mock preview does not support ${input.name}`);
}

/** Preview mocks complete synchronously, so there is nothing to stop. */
export async function mockOpPreviewCancel(_input: { previewKey: string }): Promise<void> {}

export async function mockOpInvoke(input: {
  projectId: string;
  name: string;
  input: unknown;
}): Promise<InvokeResult> {
  const { projectId, name, input: opInput } = input;
  const args = (opInput as Record<string, unknown>) ?? {};

  if (name === "asset.import") {
    const paths = (args.paths as string[]) ?? [];
    const libraryItems = (args.library_items as string[]) ?? [];
    const assetIds: string[] = [];
    const librarySources: Array<{ item: string; hash: string }> = [];
    for (const p of paths) {
      const assetId = rand("ast");
      assetIds.push(assetId);
      const filename = p.split(/[/\\]/).pop() ?? "file.txt";
      mockAssets[assetId] = {
        projectId,
        mediaType: filename.endsWith(".png")
          ? "image/png"
          : filename.endsWith(".json")
            ? "application/json"
            : "text/plain",
        content: `Preview of ${filename}\nImported from ${p}\nLine 3 sample text content`,
      };
    }
    for (const libId of libraryItems) {
      const libItem = mockLibraryItems.find((i) => i.id === libId);
      if (!libItem) {
        throw new Error(`ASSET_IMPORT/ITEM_NOT_FOUND: library item ${libId} not found`);
      }
      const assetId = rand("ast");
      assetIds.push(assetId);
      mockAssets[assetId] = {
        projectId,
        mediaType: libItem.media_type,
        content: libItem.content ?? `Content for ${libItem.name}`,
        libraryItem: libItem.id,
      };
      librarySources.push({ item: libItem.id, hash: libItem.hash });
    }
    return {
      step: {
        id: rand("step"),
        op: "asset.import",
        outcome: "succeeded",
      },
      output: { assets: assetIds, library_sources: librarySources },
    };
  }

  if (name === "text.replace") {
    const docId = typeof args.doc === "string" ? args.doc : "";
    const asset = mockAssets[docId];
    if (!asset) throw new Error(`mock asset ${docId || "(missing doc)"} not found`);
    const pairs = Array.isArray(args.pairs) ? (args.pairs as MockPair[]) : [];
    const { text, counts } = applyMockReplacements(asset.content, pairs);
    const outputId = rand("ast");
    mockAssets[outputId] = {
      projectId,
      mediaType: asset.mediaType,
      content: text,
    };
    mockAssetProvenance[outputId] = {
      op: "text.replace",
      op_version: "1.0.0",
      input: { ...args },
      output: { doc: outputId, counts },
      read_only: false,
    };
    return {
      step: { id: rand("step"), op: "text.replace", outcome: "succeeded" },
      output: { doc: outputId, counts },
    };
  }

  if (name === "text.normalize") {
    const docId = typeof args.doc === "string" ? args.doc : "";
    const asset = mockAssets[docId];
    if (!asset) throw new Error(`mock asset ${docId || "(missing doc)"} not found`);
    const text = applyMockNormalize(asset.content, args);
    const outputId = rand("ast");
    mockAssets[outputId] = {
      projectId,
      mediaType: asset.mediaType,
      content: text,
    };
    mockAssetProvenance[outputId] = {
      op: "text.normalize",
      op_version: "1.0.0",
      input: { ...args },
      output: { doc: outputId },
      read_only: false,
    };
    return {
      step: { id: rand("step"), op: "text.normalize", outcome: "succeeded" },
      output: { doc: outputId },
    };
  }

  if (name === "text.template") {
    const docId = typeof args.doc === "string" ? args.doc : "";
    const asset = mockAssets[docId];
    if (!asset) throw new Error(`mock asset ${docId || "(missing doc)"} not found`);
    const text = applyMockTemplate(asset.content, args);
    const outputId = rand("ast");
    mockAssets[outputId] = {
      projectId,
      mediaType: asset.mediaType,
      content: text,
    };
    mockAssetProvenance[outputId] = {
      op: "text.template",
      op_version: "1.0.0",
      input: { ...args },
      output: { doc: outputId },
      read_only: false,
    };
    return {
      step: { id: rand("step"), op: "text.template", outcome: "succeeded" },
      output: { doc: outputId },
    };
  }

  if (name === "text.inspect") {
    const text = mockDocumentText(projectId, args);
    const inspected = applyMockInspect(text);
    return {
      step: { id: rand("step"), op: "text.inspect", outcome: "succeeded" },
      output: inspected,
    };
  }

  if (name === "collection.create") {
    const colName = (args.name as string) || "Untitled Collection";
    const assets = (args.assets as string[]) || [];
    const collectionId = rand("col");
    const boardItemId = rand("item");

    const slots = assets.map((asset, idx) => ({
      id: `slot-${idx + 1}`,
      asset_id: asset,
    }));

    const doc: MockCollectionDoc = {
      id: collectionId,
      projectId,
      name: colName,
      version: 1,
      slots,
    };
    mockCollections[collectionId] = doc;
    mockCollectionHistory[collectionId] = { past: [], future: [] };

    const board = await mockGetBoardSnapshot({ projectId });
    board.version += 1;
    board.items.push({
      id: boardItemId,
      kind: { type: "collection", collection_id: collectionId },
      position: { x: 0, y: 0 },
      size: { width: 300, height: 200 },
      z: board.items.length + 1,
      locked: false,
    });

    emitEvent({
      type: "document",
      kind: "collection.changed",
      projectId,
      documentId: collectionId,
      version: 1,
    });
    emitEvent({
      type: "document",
      kind: "board.changed",
      projectId,
      documentId: board.id,
      version: board.version,
    });

    return {
      step: { id: rand("step"), op: "collection.create", outcome: "succeeded" },
      output: {
        collection_id: collectionId,
        board_item_id: boardItemId,
      },
    };
  }

  if (name === "collection.get") {
    const colId = (args.collection_id as string) || "";
    const doc = mockCollections[colId];
    if (!doc) {
      throw new Error(`COLLECTION/CALLER_UNKNOWN_COLLECTION: collection ${colId} not found`);
    }
    return {
      step: { id: rand("step"), op: "collection.get", outcome: "succeeded" },
      output: {
        id: doc.id,
        name: doc.name,
        version: doc.version,
        slots: doc.slots,
      },
    };
  }

  if (name === "collection.set_slot") {
    const colId = (args.collection_id as string) || "";
    const slotId = (args.slot_id as string) || "";
    const asset = (args.asset as string) || "";
    const doc = mockCollections[colId];
    if (!doc) {
      throw new Error(`COLLECTION/CALLER_UNKNOWN_COLLECTION: collection ${colId} not found`);
    }
    const history = (mockCollectionHistory[colId] = mockCollectionHistory[colId] || {
      past: [],
      future: [],
    });
    history.past.push({
      doc: JSON.parse(JSON.stringify(doc)) as MockCollectionDoc,
      recipes: snapshotCollectionRecipes(colId),
    });
    history.future = [];

    const slot = doc.slots.find((s) => s.id === slotId);
    if (!slot) {
      throw new Error(`COLLECTION/CALLER_UNKNOWN_SLOT: slot ${slotId} not found`);
    }
    const prevAssetId = slot.asset_id;
    slot.asset_id = asset;
    slot.stale = false;
    doc.version += 1;

    const prov = mockAssetProvenance[asset];
    if (prov) {
      const key = recipeKey(colId, slotId);
      const recipe = (mockSlotRecipes[key] = mockSlotRecipes[key] || {
        collection_id: colId,
        slot_id: slotId,
        root_asset_id: prevAssetId,
        steps: [],
      });
      recipe.steps.push({
        step_id: rand("step"),
        op: prov.op,
        op_version: prov.op_version,
        input: prov.input,
        input_sources: {},
        output: prov.output,
        actor: { kind: "Human", id: null, label: null },
        read_only: prov.read_only,
        stale: false,
      });
    }

    emitEvent({
      type: "document",
      kind: "collection.changed",
      projectId,
      documentId: colId,
      version: doc.version,
    });

    return {
      step: { id: rand("step"), op: "collection.set_slot", outcome: "succeeded" },
      output: { version: doc.version },
    };
  }

  if (name === "collection.undo") {
    const colId = (args.collection_id as string) || "";
    const doc = mockCollections[colId];
    if (!doc) {
      throw new Error(`COLLECTION/CALLER_UNKNOWN_COLLECTION: collection ${colId} not found`);
    }
    const history = (mockCollectionHistory[colId] = mockCollectionHistory[colId] || {
      past: [],
      future: [],
    });
    const prev = history.past.pop();
    if (prev) {
      history.future.push({
        doc: JSON.parse(JSON.stringify(doc)) as MockCollectionDoc,
        recipes: snapshotCollectionRecipes(colId),
      });
      doc.name = prev.doc.name;
      doc.slots = prev.doc.slots;
      doc.version += 1;
      restoreCollectionRecipes(colId, prev.recipes);

      emitEvent({
        type: "document",
        kind: "collection.changed",
        projectId,
        documentId: colId,
        version: doc.version,
      });
      return {
        step: { id: rand("step"), op: "collection.undo", outcome: "succeeded" },
        output: { applied: true, version: doc.version },
      };
    }
    return {
      step: { id: rand("step"), op: "collection.undo", outcome: "succeeded" },
      output: { applied: false, version: doc.version },
    };
  }

  if (name === "collection.redo") {
    const colId = (args.collection_id as string) || "";
    const doc = mockCollections[colId];
    if (!doc) {
      throw new Error(`COLLECTION/CALLER_UNKNOWN_COLLECTION: collection ${colId} not found`);
    }
    const history = (mockCollectionHistory[colId] = mockCollectionHistory[colId] || {
      past: [],
      future: [],
    });
    const next = history.future.pop();
    if (next) {
      history.past.push({
        doc: JSON.parse(JSON.stringify(doc)) as MockCollectionDoc,
        recipes: snapshotCollectionRecipes(colId),
      });
      doc.name = next.doc.name;
      doc.slots = next.doc.slots;
      doc.version += 1;
      restoreCollectionRecipes(colId, next.recipes);

      emitEvent({
        type: "document",
        kind: "collection.changed",
        projectId,
        documentId: colId,
        version: doc.version,
      });
      return {
        step: { id: rand("step"), op: "collection.redo", outcome: "succeeded" },
        output: { applied: true, version: doc.version },
      };
    }
    return {
      step: { id: rand("step"), op: "collection.redo", outcome: "succeeded" },
      output: { applied: false, version: doc.version },
    };
  }

  if (name === "recipe.get") {
    const colId = (args.collection_id as string) || "";
    const slotId = (args.slot_id as string) || "";
    const doc = mockCollections[colId];
    if (!doc) {
      throw new Error(`COLLECTION/CALLER_UNKNOWN_COLLECTION: collection ${colId} not found`);
    }
    const slot = doc.slots.find((s) => s.id === slotId);
    if (!slot) {
      throw new Error(`COLLECTION/CALLER_UNKNOWN_SLOT: slot ${slotId} not found`);
    }
    const key = recipeKey(colId, slotId);
    const recipe = mockSlotRecipes[key] ?? {
      collection_id: colId,
      slot_id: slotId,
      root_asset_id: slot.asset_id,
      steps: [],
    };
    const rootAsset = mockAssets[recipe.root_asset_id];
    const currentAsset = mockAssets[slot.asset_id];
    const encoder = new TextEncoder();
    return {
      step: { id: rand("step"), op: "recipe.get", outcome: "succeeded" },
      output: {
        collection_id: colId,
        slot_id: slotId,
        root_asset: {
          id: recipe.root_asset_id,
          media_type: rootAsset?.mediaType ?? "text/plain",
          size: rootAsset ? encoder.encode(rootAsset.content).length : 0,
        },
        current_asset: {
          id: slot.asset_id,
          media_type: currentAsset?.mediaType ?? "text/plain",
          size: currentAsset ? encoder.encode(currentAsset.content).length : 0,
        },
        stale: Boolean(slot.stale) || recipe.steps.some((s) => s.stale),
        steps: recipe.steps,
      },
    };
  }

  if (name === "recipe.edit") {
    const colId = (args.collection_id as string) || "";
    const slotId = (args.slot_id as string) || "";
    const stepId = (args.step_id as string) || "";
    const newInput = (args.input as Record<string, unknown>) ?? {};
    const doc = mockCollections[colId];
    if (!doc) {
      throw new Error(`COLLECTION/CALLER_UNKNOWN_COLLECTION: collection ${colId} not found`);
    }
    const slot = doc.slots.find((s) => s.id === slotId);
    if (!slot) {
      throw new Error(`COLLECTION/CALLER_UNKNOWN_SLOT: slot ${slotId} not found`);
    }
    const key = recipeKey(colId, slotId);
    const recipe = mockSlotRecipes[key];
    if (!recipe) {
      throw new Error(`RECIPE/STEP_NOT_FOUND: step ${stepId} not found`);
    }
    const stepIdx = recipe.steps.findIndex((s) => s.step_id === stepId);
    if (stepIdx === -1) {
      throw new Error(`RECIPE/STEP_NOT_FOUND: step ${stepId} not found`);
    }

    const history = (mockCollectionHistory[colId] = mockCollectionHistory[colId] || {
      past: [],
      future: [],
    });
    history.past.push({
      doc: JSON.parse(JSON.stringify(doc)) as MockCollectionDoc,
      recipes: snapshotCollectionRecipes(colId),
    });
    history.future = [];

    const targetStep = recipe.steps[stepIdx];
    targetStep.input = { ...targetStep.input, ...newInput };
    if (
      "values" in newInput &&
      targetStep.input_sources &&
      Object.keys(targetStep.input_sources).length > 0
    ) {
      const nextValues =
        newInput.values && typeof newInput.values === "object" && !Array.isArray(newInput.values)
          ? (newInput.values as Record<string, unknown>)
          : {};
      for (const ptr of Object.keys(targetStep.input_sources)) {
        if (ptr.startsWith("/values/")) {
          const k = ptr.slice("/values/".length);
          if (!(k in nextValues)) {
            delete targetStep.input_sources[ptr];
          }
        }
      }
    }
    const invoked = await mockOpInvoke({ projectId, name: targetStep.op, input: targetStep.input });
    targetStep.output = (invoked.output as Record<string, unknown>) ?? null;
    targetStep.stale = false;

    let latestDocId =
      typeof targetStep.output?.doc === "string"
        ? targetStep.output.doc
        : typeof targetStep.input.doc === "string"
          ? targetStep.input.doc
          : recipe.root_asset_id;
    let replayedSteps = 0;
    let staleSteps = 0;

    for (let i = stepIdx + 1; i < recipe.steps.length; i++) {
      const downstream = recipe.steps[i];
      if (
        downstream.op === "text.replace" ||
        downstream.op === "text.normalize" ||
        downstream.op === "text.template" ||
        downstream.op === "text.inspect"
      ) {
        const nextInput = { ...downstream.input, doc: latestDocId };
        downstream.input = nextInput;
        const replayed = await mockOpInvoke({ projectId, name: downstream.op, input: nextInput });
        downstream.output = (replayed.output as Record<string, unknown>) ?? null;
        downstream.stale = false;
        if (!downstream.read_only && typeof downstream.output?.doc === "string") {
          latestDocId = downstream.output.doc;
        }
        replayedSteps += 1;
      } else {
        downstream.stale = true;
        staleSteps += 1;
      }
    }

    slot.asset_id = latestDocId;
    slot.stale = staleSteps > 0;
    doc.version += 1;

    emitEvent({
      type: "document",
      kind: "collection.changed",
      projectId,
      documentId: colId,
      version: doc.version,
    });

    return {
      step: { id: rand("step"), op: "recipe.edit", outcome: "succeeded" },
      output: {
        version: doc.version,
        asset_id: slot.asset_id,
        stale: Boolean(slot.stale),
        replayed_steps: replayedSteps,
        stale_steps: staleSteps,
      },
    };
  }

  if (name === "collection.list") {
    const collections = Object.values(mockCollections)
      .filter((doc) => doc.projectId === projectId)
      .map((doc) => ({
        id: doc.id,
        name: doc.name,
        version: doc.version,
        slot_count: doc.slots.length,
      }));
    return {
      step: { id: rand("step"), op: name, outcome: "succeeded" },
      output: { collections },
    };
  }

  if (name === "dynamic_workflow.list") {
    const workflows = MOCK_DYNAMIC_WORKFLOWS.filter(
      (workflow) => workflow.projectId === projectId,
    ).map((workflow) => ({
      id: workflow.id,
      name: workflow.name,
      version: workflow.version,
      step_count: workflow.step_count,
    }));
    return {
      step: { id: rand("step"), op: name, outcome: "succeeded" },
      output: { workflows },
    };
  }

  if (name === "asset.list") {
    const assets = Object.entries(mockAssets)
      .reverse()
      .filter(([, asset]) => asset.projectId === projectId)
      .map(([id, asset]) => ({
        id,
        media_type: asset.mediaType,
        size: asset.content.length,
        origin: "step-mock",
        ...(asset.libraryItem ? { library_item: asset.libraryItem } : {}),
      }));
    return {
      step: { id: rand("step"), op: name, outcome: "succeeded" },
      output: { assets },
    };
  }

  if (name === "activity.list") {
    const limit = typeof args.limit === "number" ? Math.max(1, Math.floor(args.limit)) : 50;
    const before = typeof args.before === "string" ? args.before : null;
    const all = mockActivityFor(projectId);
    let start = 0;
    if (before) {
      const cursor = all.findIndex((record) => record.id === before);
      // Mirrors the host: an unknown cursor is an empty final page, never a
      // silent jump back to the newest records.
      if (cursor === -1) {
        return {
          step: { id: rand("step"), op: name, outcome: "succeeded" },
          output: { records: [], has_more: false },
        };
      }
      start = cursor + 1;
    }
    return {
      step: { id: rand("step"), op: name, outcome: "succeeded" },
      output: {
        records: all.slice(start, start + limit),
        has_more: start + limit < all.length,
      },
    };
  }

  if (name === "dynamic_workflow.list_runs") {
    const collectionId = args.collection_id as string | undefined;
    const limit = typeof args.limit === "number" ? Math.max(1, args.limit) : 50;
    // The demo run belongs to the fixture project's collection; other projects
    // and other collections have no runs, matching the old empty behavior.
    const hasDemoRun =
      projectId === MOCK_FIXTURE_PROJECT_ID &&
      (!collectionId || collectionId === "col_demo_drafts");
    if (!hasDemoRun) {
      return {
        step: { id: rand("step"), op: name, outcome: "succeeded" },
        output: { runs: [] },
      };
    }
    return {
      step: { id: rand("step"), op: name, outcome: "succeeded" },
      output: { runs: [mockDemoRun(projectId)].slice(0, limit) },
    };
  }

  if (name === "library.list") {
    const nameContains =
      typeof args.name_contains === "string" ? args.name_contains.toLowerCase() : null;
    const tag = typeof args.tag === "string" ? args.tag.toLowerCase() : null;
    const mediaType = typeof args.media_type === "string" ? args.media_type.toLowerCase() : null;
    const includeRemoved = Boolean(args.include_removed);

    const items = mockLibraryItems.filter((item) => {
      if (!includeRemoved && item.removed) return false;
      if (nameContains && !item.name.toLowerCase().includes(nameContains)) return false;
      if (tag && !item.tags.some((t) => t.toLowerCase() === tag)) return false;
      if (mediaType && !item.media_type.toLowerCase().startsWith(mediaType)) return false;
      return true;
    });

    return {
      step: { id: rand("step"), op: name, outcome: "succeeded" },
      output: { items },
    };
  }

  if (name === "library.add") {
    const paths = (args.paths as string[]) ?? [];
    mockLibraryHistory.past.push(JSON.parse(JSON.stringify(mockLibraryItems)));
    mockLibraryHistory.future = [];
    const addedItems: Array<{ id: string; name: string; deduplicated: boolean }> = [];

    for (const p of paths) {
      const filename = p.split(/[/\\\\]/).pop() ?? "file";
      const existing = mockLibraryItems.find(
        (i) => !i.removed && (i.source as { path?: string })?.path === p,
      );
      if (existing) {
        addedItems.push({ id: existing.id, name: existing.name, deduplicated: true });
      } else {
        const id = rand("lib");
        const ext = filename.split(".").pop()?.toLowerCase();
        const media_type =
          ext === "png" || ext === "jpg" || ext === "jpeg" || ext === "webp"
            ? `image/${ext === "jpg" ? "jpeg" : ext}`
            : ext === "mp4" || ext === "webm"
              ? `video/${ext}`
              : ext === "mp3" || ext === "wav"
                ? `audio/${ext}`
                : ext === "json"
                  ? "application/json"
                  : "text/plain";
        const newItem: MockLibraryItem = {
          id,
          name: filename,
          tags: [],
          media_type,
          size: 1024,
          hash: rand("hash"),
          added_at: new Date().toISOString(),
          removed: false,
          source: { type: "local", path: p },
          content: `Content of ${filename}`,
        };
        mockLibraryItems.push(newItem);
        addedItems.push({ id, name: filename, deduplicated: false });
      }
    }
    mockLibraryVersion += 1;
    emitEvent({
      type: "document",
      kind: "library.changed",
      projectId: "library",
      documentId: "library",
      version: mockLibraryVersion,
    });
    return {
      step: { id: rand("step"), op: name, outcome: "succeeded" },
      output: { items: addedItems, version: mockLibraryVersion },
    };
  }

  if (name === "library.rename") {
    const id = args.id as string;
    const nextName = args.name as string;
    const item = mockLibraryItems.find((i) => i.id === id);
    if (!item) throw new Error(`LIBRARY/NOT_FOUND: item ${id} not found`);
    mockLibraryHistory.past.push(JSON.parse(JSON.stringify(mockLibraryItems)));
    mockLibraryHistory.future = [];
    item.name = nextName;
    mockLibraryVersion += 1;
    emitEvent({
      type: "document",
      kind: "library.changed",
      projectId: "library",
      documentId: "library",
      version: mockLibraryVersion,
    });
    return {
      step: { id: rand("step"), op: name, outcome: "succeeded" },
      output: { version: mockLibraryVersion },
    };
  }

  if (name === "library.tag") {
    const id = args.id as string;
    const tags = (args.tags as string[]) ?? [];
    const item = mockLibraryItems.find((i) => i.id === id);
    if (!item) throw new Error(`LIBRARY/NOT_FOUND: item ${id} not found`);
    mockLibraryHistory.past.push(JSON.parse(JSON.stringify(mockLibraryItems)));
    mockLibraryHistory.future = [];
    item.tags = [...tags];
    mockLibraryVersion += 1;
    emitEvent({
      type: "document",
      kind: "library.changed",
      projectId: "library",
      documentId: "library",
      version: mockLibraryVersion,
    });
    return {
      step: { id: rand("step"), op: name, outcome: "succeeded" },
      output: { version: mockLibraryVersion },
    };
  }

  if (name === "library.remove") {
    const ids = (args.ids as string[]) ?? [];
    mockLibraryHistory.past.push(JSON.parse(JSON.stringify(mockLibraryItems)));
    mockLibraryHistory.future = [];
    for (const id of ids) {
      const item = mockLibraryItems.find((i) => i.id === id);
      if (item) item.removed = true;
    }
    mockLibraryVersion += 1;
    emitEvent({
      type: "document",
      kind: "library.changed",
      projectId: "library",
      documentId: "library",
      version: mockLibraryVersion,
    });
    return {
      step: { id: rand("step"), op: name, outcome: "succeeded" },
      output: { version: mockLibraryVersion },
    };
  }

  if (name === "library.undo") {
    const prev = mockLibraryHistory.past.pop();
    if (prev) {
      mockLibraryHistory.future.push(JSON.parse(JSON.stringify(mockLibraryItems)));
      mockLibraryItems.length = 0;
      mockLibraryItems.push(...prev);
      mockLibraryVersion += 1;
      emitEvent({
        type: "document",
        kind: "library.changed",
        projectId: "library",
        documentId: "library",
        version: mockLibraryVersion,
      });
      return {
        step: { id: rand("step"), op: name, outcome: "succeeded" },
        output: { applied: true, version: mockLibraryVersion },
      };
    }
    return {
      step: { id: rand("step"), op: name, outcome: "succeeded" },
      output: { applied: false, version: mockLibraryVersion },
    };
  }

  if (name === "library.redo") {
    const next = mockLibraryHistory.future.pop();
    if (next) {
      mockLibraryHistory.past.push(JSON.parse(JSON.stringify(mockLibraryItems)));
      mockLibraryItems.length = 0;
      mockLibraryItems.push(...next);
      mockLibraryVersion += 1;
      emitEvent({
        type: "document",
        kind: "library.changed",
        projectId: "library",
        documentId: "library",
        version: mockLibraryVersion,
      });
      return {
        step: { id: rand("step"), op: name, outcome: "succeeded" },
        output: { applied: true, version: mockLibraryVersion },
      };
    }
    return {
      step: { id: rand("step"), op: name, outcome: "succeeded" },
      output: { applied: false, version: mockLibraryVersion },
    };
  }

  if (name === "library.collect") {
    const itemsInput = (args.items as Array<{ asset: string; name?: string }>) ?? [];
    mockLibraryHistory.past.push(JSON.parse(JSON.stringify(mockLibraryItems)));
    mockLibraryHistory.future = [];
    const collected: Array<{ id: string; name: string }> = [];

    for (const it of itemsInput) {
      const asset = mockAssets[it.asset];
      const assetName = it.name || it.asset;
      const id = rand("lib");
      const newItem: MockLibraryItem = {
        id,
        name: assetName,
        tags: [],
        media_type: asset?.mediaType ?? "application/octet-stream",
        size: asset?.content?.length ?? 1024,
        hash: rand("hash"),
        added_at: new Date().toISOString(),
        removed: false,
        source: { type: "project_asset", project: projectId, asset: it.asset },
        content: asset?.content,
      };
      mockLibraryItems.push(newItem);
      collected.push({ id, name: assetName });
    }
    mockLibraryVersion += 1;
    emitEvent({
      type: "document",
      kind: "library.changed",
      projectId: "library",
      documentId: "library",
      version: mockLibraryVersion,
    });
    return {
      step: { id: rand("step"), op: name, outcome: "succeeded" },
      output: { items: collected, version: mockLibraryVersion },
    };
  }

  if (name === "project.rename") {
    const newName = typeof args.name === "string" ? args.name.trim() : "";
    if (!newName) {
      throw new Error("PROJECT/CALLER_EMPTY_NAME: project name must not be empty");
    }
    const projects = mockReadProjects();
    const project = projects[projectId];
    if (!project) throw new Error("mock project not found: " + projectId);
    const updatedAt = new Date().toISOString();
    projects[projectId] = { ...project, name: newName, updatedAt };
    mockWriteProjects(projects);
    return {
      step: { id: rand("step"), op: name, outcome: "succeeded" },
      output: { name: newName, updated_at: updatedAt },
    };
  }

  if (name === "project.list_folders") {
    const folders = mockWorkspaceFolders[projectId] ?? [];
    return {
      step: { id: rand("step"), op: name, outcome: "succeeded" },
      output: { folders },
    };
  }

  if (name === "project.add_folder") {
    const path = typeof args.path === "string" ? args.path.trim() : "";
    if (!path) {
      throw new Error("PROJECT/INVALID_PATH: path cannot be empty");
    }
    if (path.includes(".reimagine") || path.includes("app_data")) {
      throw new Error(
        "PROJECT/FOLDER_IN_APP_DATA: workspace folder cannot be inside application data directory",
      );
    }
    const folders = mockWorkspaceFolders[projectId] ?? (mockWorkspaceFolders[projectId] = []);
    if (folders.some((f) => f.path === path)) {
      throw new Error("PROJECT/DUPLICATE_FOLDER: folder already added to workspace");
    }
    const folder = { id: rand("fld"), path };
    folders.push(folder);
    return {
      step: { id: rand("step"), op: name, outcome: "succeeded" },
      output: { folder },
    };
  }

  if (name === "project.remove_folder") {
    const id = typeof args.id === "string" ? args.id : "";
    const folders = mockWorkspaceFolders[projectId] ?? [];
    const idx = folders.findIndex((f) => f.id === id);
    if (idx !== -1) {
      folders.splice(idx, 1);
    }
    return {
      step: { id: rand("step"), op: name, outcome: "succeeded" },
      output: {},
    };
  }

  return {
    step: { id: rand("step"), op: name, outcome: "succeeded" },
    output: {},
  };
}
