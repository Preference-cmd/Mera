import {
  type ActivityRecord,
  type BoardCommandResultDto,
  type BoardItem,
  type BoardSnapshot,
  type Event,
  type Project,
  type ProjectMetadataInput,
  type InvokeResult,
  type ModBrowseResponseDto,
  type ModDownloadTaskDto,
  type ModEntryDto,
  type ModInstallPreviewDto,
  type ModListResponseDto,
  type ModOperationSummaryDto,
  type ModPermissions,
  type ModRuntimeEnvironmentDto,
  type ModSourceDto,
  type ModSourcesListDto,
  type OperationInfo,
  type RemoteModCatalogItemDto,
  type StorageCleanCacheResult,
  type StoragePreflightWriteInput,
  type StoragePreflightWriteResult,
  type StorageStatusDto,
  type ThirdPartySourceDto,
  type WorkflowSpec,
} from "./schemas";

const delay = (ms: number) => new Promise((r) => setTimeout(r, ms));

function rand(prefix: string) {
  return `${prefix}_${Math.random().toString(36).slice(2, 10)}`;
}

/* ───── Project mocks (localStorage-backed) ───── */

const PROJECTS_KEY = "mera.mock.projects";
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
  name?: string;
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
  default: [{ id: "fld_1", path: "/Users/pref2rence/project/Mera/sample_workspace" }],
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
    name: "style_guide.md",
    mediaType: "text/markdown",
    content:
      "# Style guide\n\nVoice, headings and image ratios for the workspace.\n\n" +
      "Use sentence case for headings and plain verbs for actions.\n".repeat(40),
  },
  ast_demo_blog_v2: {
    projectId: MOCK_FIXTURE_PROJECT_ID,
    name: "blog_v2.md",
    mediaType: "text/markdown",
    content:
      "# Blog v2\n\nOutline and opening paragraph.\n\n" +
      "The draft keeps one idea per section and cites its source.\n".repeat(28),
  },
  ast_demo_cover_brief: {
    projectId: MOCK_FIXTURE_PROJECT_ID,
    name: "cover_brief.txt",
    mediaType: "text/plain",
    content:
      "Cover brief: warm grey background, one image, no headline.\n" +
      "Keep the lower third clear for the series label.\n".repeat(55),
  },
};

const BUILTIN_DYNAMIC_WORKFLOW_OPS = new Set([
  "dynamic_workflow.create",
  "dynamic_workflow.get",
  "dynamic_workflow.list",
  "dynamic_workflow.run",
  "dynamic_workflow.cancel",
  "dynamic_workflow.resume",
  "dynamic_workflow.get_run",
  "dynamic_workflow.list_runs",
  "dynamic_workflow.from_recipe",
]);

type MockDynamicWorkflowDoc = {
  id: string;
  projectId: string;
  name: string;
  description?: string;
  version: number;
  workflow: WorkflowSpec;
};

const mockDynamicWorkflows: MockDynamicWorkflowDoc[] = [
  {
    id: "dwf_demo_cleanup",
    projectId: MOCK_FIXTURE_PROJECT_ID,
    name: "Draft cleanup",
    description: "Normalize whitespace, fix smart quotes, and inspect document stats.",
    version: 3,
    workflow: {
      inputs: {
        doc: { type: "string", format: "asset", contentMediaType: "text/*" },
      },
      params: { type: "object" },
      steps: [
        {
          id: "step_1",
          op: "text.normalize",
          input: {
            doc: { $from: "inputs.doc" },
            whitespace: true,
            newline: true,
            punctuation: "halfwidth",
            unicode: "NFC",
          },
        },
        {
          id: "step_2",
          op: "text.replace",
          input: {
            doc: { $from: "steps.step_1.doc" },
            pairs: [{ find: "TODO", replace: "DONE", mode: "word", case_sensitive: true }],
          },
        },
        {
          id: "step_3",
          op: "text.inspect",
          input: {
            doc: { $from: "steps.step_2.doc" },
          },
        },
      ],
      output: {
        doc: { $from: "steps.step_2.doc" },
      },
    },
  },
  {
    id: "dwf_demo_archive",
    projectId: MOCK_FIXTURE_PROJECT_ID,
    name: "Archive sweep",
    description: "Normalize line endings and apply archive header template.",
    version: 1,
    workflow: {
      inputs: {
        doc: { type: "string", format: "asset", contentMediaType: "text/*" },
      },
      params: { type: "object" },
      steps: [
        {
          id: "step_1",
          op: "text.normalize",
          input: {
            doc: { $from: "inputs.doc" },
            whitespace: true,
            newline: true,
          },
        },
        {
          id: "step_2",
          op: "text.template",
          input: {
            doc: { $from: "steps.step_1.doc" },
            template: "[ARCHIVED]\n{{body}}",
            values: {},
          },
        },
      ],
      output: {
        doc: { $from: "steps.step_2.doc" },
      },
    },
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
  const base = MOCK_TEXT_OPERATIONS.map((operation) => ({
    ...operation,
    spec: { ...operation.spec },
  }));
  const dynamicOps: OperationInfo[] = mockDynamicWorkflows
    .filter((wf) => wf.projectId === input.projectId)
    .map((wf) => {
      const inputKeys = Object.keys(wf.workflow.inputs);
      const paramsSchema = wf.workflow.params as { required?: unknown };
      const paramsHasRequired =
        Array.isArray(paramsSchema?.required) && paramsSchema.required.length > 0;
      return {
        spec: {
          name: `dynamic_workflow.${wf.id}`,
          description: wf.description || wf.name,
          input_schema: {
            type: "object",
            properties: {
              ...wf.workflow.inputs,
              params: wf.workflow.params,
            },
            required: [...inputKeys, ...(paramsHasRequired ? ["params"] : [])],
            additionalProperties: false,
          },
          output_schema: {
            type: "object",
            properties: {
              output: { type: "string", format: "asset" },
            },
            required: ["output"],
            additionalProperties: false,
          },
          annotations: {
            read_only: false,
            destructive: false,
            idempotent: false,
            deterministic: true,
            open_world: false,
          },
        },
        provider: { dynamic_workflow: { id: wf.id } },
        health: { status: "available" },
      };
    });
  const modOps = buildEnabledMockModOperations();
  return [...base, ...modOps, ...dynamicOps];
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
    const librarySources: Array<{ item: string; hash: string; name: string }> = [];
    for (const p of paths) {
      const assetId = rand("ast");
      assetIds.push(assetId);
      const filename = p.split(/[/\\]/).pop() ?? "file.txt";
      mockAssets[assetId] = {
        projectId,
        name: filename,
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
        name: libItem.name,
        mediaType: libItem.media_type,
        content: libItem.content ?? `Content for ${libItem.name}`,
        libraryItem: libItem.id,
      };
      librarySources.push({ item: libItem.id, hash: libItem.hash, name: libItem.name });
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
      name: "text.replace #1",
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
    const workflows = mockDynamicWorkflows
      .filter((workflow) => workflow.projectId === projectId)
      .map((workflow) => ({
        id: workflow.id,
        name: workflow.name,
        version: workflow.version,
        step_count: workflow.workflow.steps.length,
      }));
    return {
      step: { id: rand("step"), op: name, outcome: "succeeded" },
      output: { workflows },
    };
  }

  if (name === "dynamic_workflow.get") {
    const wfId = typeof args.dynamic_workflow_id === "string" ? args.dynamic_workflow_id : "";
    const wf = mockDynamicWorkflows.find(
      (workflow) => workflow.id === wfId && workflow.projectId === projectId,
    );
    if (!wf) {
      throw new Error(`DYNAMIC_WORKFLOW/NOT_FOUND: dynamic workflow ${wfId} not found`);
    }
    return {
      step: { id: rand("step"), op: name, outcome: "succeeded" },
      output: {
        id: wf.id,
        name: wf.name,
        ...(wf.description ? { description: wf.description } : {}),
        version: wf.version,
        workflow: wf.workflow,
      },
    };
  }

  if (name === "dynamic_workflow.create") {
    const wfName = typeof args.name === "string" ? args.name.trim() : "";
    if (!wfName) {
      throw new Error("DYNAMIC_WORKFLOW/INVALID_NAME: workflow name must not be empty");
    }
    const wfDesc = typeof args.description === "string" ? args.description : undefined;
    const rawWorkflow = (args.workflow as WorkflowSpec | undefined) ?? {
      inputs: {},
      params: { type: "object" },
      steps: [],
      output: {},
    };
    const id = rand("dwf");
    const doc: MockDynamicWorkflowDoc = {
      id,
      projectId,
      name: wfName,
      ...(wfDesc ? { description: wfDesc } : {}),
      version: 1,
      workflow: rawWorkflow,
    };
    mockDynamicWorkflows.push(doc);
    emitEvent({
      type: "document",
      kind: "dynamic_workflow.changed",
      projectId,
      documentId: id,
      version: 1,
    });
    return {
      step: { id: rand("step"), op: name, outcome: "succeeded" },
      output: { dynamic_workflow_id: id },
    };
  }

  if (name === "dynamic_workflow.from_recipe") {
    const colId = typeof args.collection_id === "string" ? args.collection_id : "";
    const slotId = typeof args.slot_id === "string" ? args.slot_id : "";
    const doc = mockCollections[colId];
    if (!doc) {
      throw new Error(`COLLECTION/CALLER_UNKNOWN_COLLECTION: collection ${colId} not found`);
    }
    const slot = doc.slots.find((s) => s.id === slotId);
    if (!slot) {
      throw new Error(`COLLECTION/CALLER_UNKNOWN_SLOT: slot ${slotId} not found`);
    }
    const recipe = mockSlotRecipes[recipeKey(colId, slotId)];
    if (!recipe || recipe.steps.length === 0) {
      throw new Error("RECIPE/EMPTY: slot has no recipe steps to freeze");
    }
    if (slot.stale || recipe.steps.some((s) => s.stale)) {
      throw new Error("RECIPE/STALE: stale recipe cannot be frozen");
    }

    let prevDocStepId: string | null = null;
    const steps = recipe.steps.map((s, index) => {
      const stepId = `step_${index + 1}`;
      const stepInput: Record<string, unknown> = { ...s.input };
      if ("doc" in stepInput) {
        stepInput.doc = prevDocStepId
          ? { $from: `steps.${prevDocStepId}.doc` }
          : { $from: "inputs.doc" };
      }
      if (!s.read_only) {
        prevDocStepId = stepId;
      }
      return { id: stepId, op: s.op, input: stepInput };
    });

    const workflow: WorkflowSpec = {
      inputs: {
        doc: { type: "string", format: "asset", contentMediaType: "text/*" },
      },
      params: { type: "object" },
      steps,
      output: {
        doc: { $from: prevDocStepId ? `steps.${prevDocStepId}.doc` : "inputs.doc" },
      },
    };

    const wfName =
      typeof args.name === "string" && args.name.trim()
        ? args.name.trim()
        : `${doc.name} (${slotId})`;
    const wfDesc = typeof args.description === "string" ? args.description : undefined;
    const shouldSave = args.save !== false;

    if (shouldSave) {
      const id = rand("dwf");
      mockDynamicWorkflows.push({
        id,
        projectId,
        name: wfName,
        ...(wfDesc ? { description: wfDesc } : {}),
        version: 1,
        workflow,
      });
      emitEvent({
        type: "document",
        kind: "dynamic_workflow.changed",
        projectId,
        documentId: id,
        version: 1,
      });
      return {
        step: { id: rand("step"), op: name, outcome: "succeeded" },
        output: { dynamic_workflow_id: id, name: wfName, workflow },
      };
    }

    return {
      step: { id: rand("step"), op: name, outcome: "succeeded" },
      output: { name: wfName, workflow },
    };
  }

  if (name === "asset.list") {
    const assets = Object.entries(mockAssets)
      .reverse()
      .filter(([, asset]) => asset.projectId === projectId)
      .map(([id, asset]) => ({
        id,
        name: asset.name ?? id,
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

  if (name === "library.purge") {
    const removedItems = mockLibraryItems.filter((item) => item.removed);
    const activeHashes = new Set(
      mockLibraryItems.filter((item) => !item.removed).map((item) => item.hash),
    );
    const purgedHashes = new Set<string>();
    let freedBytes = 0;
    for (const item of removedItems) {
      if (!activeHashes.has(item.hash) && !purgedHashes.has(item.hash)) {
        purgedHashes.add(item.hash);
        freedBytes += item.size;
      }
    }
    const kept = mockLibraryItems.filter((item) => !item.removed);
    mockLibraryItems.length = 0;
    mockLibraryItems.push(...kept);
    mockLibraryHistory.past.length = 0;
    mockLibraryHistory.future.length = 0;
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
      output: {
        purged_entries: removedItems.length,
        deleted_files: purgedHashes.size,
        freed_bytes: freedBytes,
        version: mockLibraryVersion,
      },
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

  if (name === "library.place") {
    const libraryItems = Array.isArray(args.library_items) ? (args.library_items as string[]) : [];
    const pos = (args.position as { x?: number; y?: number } | undefined) ?? {};
    const centerX = typeof pos.x === "number" && Number.isFinite(pos.x) ? pos.x : 0;
    const centerY = typeof pos.y === "number" && Number.isFinite(pos.y) ? pos.y : 0;
    const customName = typeof args.name === "string" ? args.name.trim() : "";

    if (libraryItems.length === 0) {
      throw new Error("LIBRARY/INVALID_INPUT: library_items must not be empty");
    }

    const resolved = libraryItems.map((libId) => {
      const libItem = mockLibraryItems.find((i) => i.id === libId && !i.removed);
      if (!libItem) {
        throw new Error(`LIBRARY/ENTRY_NOT_FOUND: library item ${libId} not found`);
      }
      return libItem;
    });

    const assetIds: string[] = [];
    const librarySources: Array<{ item: string; hash: string; name: string }> = [];
    for (const libItem of resolved) {
      const assetId = rand("ast");
      assetIds.push(assetId);
      mockAssets[assetId] = {
        projectId,
        name: libItem.name,
        mediaType: libItem.media_type,
        content: libItem.content ?? `Content for ${libItem.name}`,
        libraryItem: libItem.id,
      };
      librarySources.push({ item: libItem.id, hash: libItem.hash, name: libItem.name });
    }

    const board = await mockGetBoardSnapshot({ projectId });
    const history = mockBoardHistory[projectId]!;
    const boardItemId = rand("item");

    let collectionId: string | null = null;
    let kind: BoardItem["kind"];
    let size: { width: number; height: number };

    if (assetIds.length === 1) {
      kind = { type: "asset_ref", artifact_id: assetIds[0] };
      size = { width: 240, height: 160 };
    } else {
      collectionId = rand("col");
      const colName = customName || `${resolved[0].name} (+${resolved.length - 1})`;
      mockCollections[collectionId] = {
        id: collectionId,
        projectId,
        name: colName,
        version: 1,
        slots: assetIds.map((aid, idx) => ({ id: `slot-${idx + 1}`, asset_id: aid })),
      };
      kind = { type: "collection", collection_id: collectionId };
      size = { width: 300, height: 200 };
    }

    const newItem: BoardItem = {
      id: boardItemId,
      kind,
      position: {
        x: Math.round(centerX - size.width / 2),
        y: Math.round(centerY - size.height / 2),
      },
      size,
      z: board.items.length,
      locked: false,
    };

    const entryId = `board-history:${rand("batch")}`;
    history.past.push({
      items: JSON.parse(JSON.stringify(board.items)),
      entryId,
      targets: [boardItemId],
    });
    history.future = [];
    board.items.push(newItem);
    board.version += 1;

    emitEvent({
      type: "document",
      kind: "board.changed",
      projectId,
      documentId: board.id,
      version: board.version,
    });
    if (collectionId) {
      emitEvent({
        type: "document",
        kind: "collection.changed",
        projectId,
        documentId: collectionId,
        version: 1,
      });
    }

    const firstName = resolved[0].name;
    appendMockActivity(projectId, {
      actor: { kind: "Human", id: null, label: null },
      summary:
        resolved.length === 1
          ? `Imported ${firstName} from library`
          : `Imported ${resolved.length} files from library`,
      summaryKey: "activity.library.place",
      summaryParams: { count: resolved.length, name: firstName },
      targets: [boardItemId],
      kind: "applied",
      historyEntryId: entryId,
    });

    return {
      step: { id: rand("step"), op: "library.place", outcome: "succeeded" },
      output: {
        assets: assetIds,
        collection_id: collectionId,
        board_item_id: boardItemId,
        history_entry_id: entryId,
        library_sources: librarySources,
      },
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
    if (path.includes(".mera") || path.includes("app_data")) {
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

  if (name.startsWith("dynamic_workflow.") && !BUILTIN_DYNAMIC_WORKFLOW_OPS.has(name)) {
    const wfId = name.slice("dynamic_workflow.".length);
    const wf = mockDynamicWorkflows.find(
      (workflow) => workflow.id === wfId && workflow.projectId === projectId,
    );
    if (!wf) {
      throw new Error(`DYNAMIC_WORKFLOW/NOT_FOUND: dynamic workflow ${wfId} not found`);
    }
    const stepOutputs: Record<string, Record<string, unknown>> = {};
    let finalAssetId = typeof args.doc === "string" ? args.doc : "";
    for (const wfStep of wf.workflow.steps) {
      const resolvedInput: Record<string, unknown> = {};
      for (const [k, v] of Object.entries(wfStep.input)) {
        if (v && typeof v === "object" && !Array.isArray(v) && "$from" in v) {
          const from = String((v as { $from: unknown }).$from);
          if (from.startsWith("inputs.")) {
            resolvedInput[k] = args[from.slice("inputs.".length)];
          } else if (from.startsWith("steps.")) {
            const [, refStepId, refField] = from.split(".");
            resolvedInput[k] = stepOutputs[refStepId]?.[refField];
          }
        } else {
          resolvedInput[k] = v;
        }
      }
      const res = await mockOpInvoke({ projectId, name: wfStep.op, input: resolvedInput });
      const out = (res.output as Record<string, unknown>) ?? {};
      stepOutputs[wfStep.id] = out;
      if (typeof out.doc === "string") {
        finalAssetId = out.doc;
      }
    }
    return {
      step: { id: rand("step"), op: name, outcome: "succeeded" },
      output: { output: finalAssetId },
    };
  }

  return {
    step: { id: rand("step"), op: name, outcome: "succeeded" },
    output: {},
  };
}

/* ───── Mod Manager (M2) ───── */

const MOCK_MODS_ROOT = "/mock/.mera/mods";

type MockModUpgradeSpec = {
  version: string;
  permissions: ModPermissions;
  operations: ModOperationSummaryDto[];
};

const mockInstalledMods: ModEntryDto[] = [];
const mockStagedLocalUpgrades = new Map<string, MockModUpgradeSpec>();

function cloneModPermissions(p: ModPermissions): ModPermissions {
  return {
    host_paths: p.host_paths,
    events_subscribe: [...p.events_subscribe],
    events_publish: [...p.events_publish],
    recommended_agent_operations: [...p.recommended_agent_operations],
    network: p.network,
    filesystem_note: p.filesystem_note ?? null,
  };
}

function cloneModEntry(entry: ModEntryDto): ModEntryDto {
  return {
    ...entry,
    source: { ...entry.source },
    health: { ...entry.health },
    permissions: cloneModPermissions(entry.permissions),
    operations: entry.operations.map((op) => ({ ...op })),
    agent_visible_operations: [...entry.agent_visible_operations],
  };
}

function bundledTextModEntry(): ModEntryDto {
  const operations: ModOperationSummaryDto[] = MOCK_TEXT_OPERATIONS.map((op) => {
    const annotations = (op.spec.annotations ?? {}) as Record<string, unknown>;
    return {
      name: op.spec.name,
      description: op.spec.description,
      read_only: annotations.read_only === true,
      destructive: annotations.destructive === true,
      idempotent: annotations.idempotent === true,
      deterministic: annotations.deterministic === true,
      open_world: annotations.open_world === true,
      uses_host_path: false,
      recommended_for_agent: false,
    };
  });
  return {
    id: "text",
    name: "Text Mod",
    version: "0.1.0",
    api: "1",
    author: null,
    homepage: null,
    license: null,
    entrypoint_kind: "binary",
    source: { kind: "bundled" },
    install_dir: null,
    package_sha256: null,
    installed_at: null,
    enabled: true,
    read_only_bundled: true,
    health: { status: "available", diagnostic_code: null, message: null },
    sandbox: {
      backend: "bundled_process",
      enforced: false,
      network_allowed: false,
      host_paths_allowed: false,
    },
    permissions: {
      host_paths: false,
      events_subscribe: [],
      events_publish: [],
      recommended_agent_operations: [],
      network: false,
      filesystem_note: null,
    },
    operations,
    agent_visible_operations: operations.map((op) => op.name),
  };
}

function defaultThirdPartyOperations(includeRemote: boolean): ModOperationSummaryDto[] {
  const base: ModOperationSummaryDto[] = [
    {
      name: "markdown_tools.callout",
      description: "Wrap selected paragraphs into a clean Markdown callout block.",
      read_only: false,
      destructive: false,
      idempotent: true,
      deterministic: true,
      open_world: false,
      uses_host_path: false,
      recommended_for_agent: true,
    },
    {
      name: "markdown_tools.lint",
      description: "Inspect heading structure and broken Markdown links.",
      read_only: true,
      destructive: false,
      idempotent: true,
      deterministic: true,
      open_world: false,
      uses_host_path: false,
      recommended_for_agent: false,
    },
  ];
  if (includeRemote) {
    base.push({
      name: "markdown_tools.export_remote",
      description: "Publish rendered Markdown notes to a configured remote endpoint.",
      read_only: false,
      destructive: false,
      idempotent: false,
      deterministic: false,
      open_world: true,
      uses_host_path: false,
      recommended_for_agent: true,
    });
  }
  return base;
}

function buildMockModFixture(
  path: string,
  mode: "archive" | "directory",
): {
  id: string;
  name: string;
  version: string;
  api: string;
  target: string;
  author: string;
  homepage: string;
  license: string;
  source: ModSourceDto;
  package_sha256: string | null;
  permissions: ModPermissions;
  operations: ModOperationSummaryDto[];
} {
  const existing = mockInstalledMods.find((entry) => entry.id === "markdown_tools");
  const staged =
    mode === "directory" && existing?.source.kind === "local_directory"
      ? mockStagedLocalUpgrades.get("markdown_tools")
      : undefined;
  const isUpgradeFixture = path.includes("upgrade");
  const version = staged?.version ?? (isUpgradeFixture ? "0.2.0" : "0.1.0");
  const operations = staged?.operations
    ? staged.operations.map((op) => ({ ...op }))
    : defaultThirdPartyOperations(isUpgradeFixture);
  const permissions: ModPermissions = staged?.permissions
    ? cloneModPermissions(staged.permissions)
    : {
        host_paths: false,
        events_subscribe: [],
        events_publish: [],
        recommended_agent_operations: isUpgradeFixture
          ? ["markdown_tools.callout", "markdown_tools.export_remote"]
          : ["markdown_tools.callout"],
        network: isUpgradeFixture,
        filesystem_note: null,
      };
  const filename = path.split(/[/\\]/).pop() || path;
  const source: ModSourceDto =
    mode === "archive" ? { kind: "local_archive", filename } : { kind: "local_directory", path };
  const package_sha256 =
    mode === "archive" ? (isUpgradeFixture ? "b".repeat(64) : "a".repeat(64)) : null;

  return {
    id: "markdown_tools",
    name: "Markdown Tools",
    version,
    api: "1",
    target: "aarch64-apple-darwin",
    author: "Mera Community",
    homepage: "https://example.invalid/markdown-tools",
    license: "MIT",
    source,
    package_sha256,
    permissions,
    operations,
  };
}

function computePermissionExpansions(
  existing: ModPermissions | undefined,
  next: ModPermissions,
): string[] {
  if (!existing) return [];
  const reasons: string[] = [];
  if (!existing.host_paths && next.host_paths) {
    reasons.push("host path access requested");
  }
  if (!existing.network && next.network) {
    reasons.push("network access requested");
  }
  for (const kind of next.events_subscribe) {
    if (!existing.events_subscribe.includes(kind)) {
      reasons.push(`new event subscription: ${kind}`);
    }
  }
  for (const kind of next.events_publish) {
    if (!existing.events_publish.includes(kind)) {
      reasons.push(`new event publication: ${kind}`);
    }
  }
  return reasons;
}

function buildPreviewFromFixture(
  path: string,
  mode: "archive" | "directory",
): ModInstallPreviewDto {
  const fixture = buildMockModFixture(path, mode);
  const existing = mockInstalledMods.find((entry) => entry.id === fixture.id);
  const is_upgrade = Boolean(existing);

  let source_conflict: string | null = null;
  if (path.includes("conflict")) {
    source_conflict = `mod '${fixture.id}' is already installed from another source; uninstall it before installing from a new source`;
  } else if (existing && existing.source.kind !== fixture.source.kind) {
    source_conflict = `mod '${fixture.id}' is already installed from another source; uninstall it before installing from a new source`;
  }

  const permission_expansions = computePermissionExpansions(
    existing?.permissions,
    fixture.permissions,
  );
  const existingNames = new Set(existing?.operations.map((op) => op.name) ?? []);
  const newly_added_operations = existing
    ? fixture.operations.filter((op) => !existingNames.has(op.name)).map((op) => op.name)
    : [];
  const nextNames = new Set(fixture.operations.map((op) => op.name));
  const existing_agent_visible_operations = existing
    ? existing.agent_visible_operations.filter((name) => nextNames.has(name))
    : [];

  return {
    id: fixture.id,
    name: fixture.name,
    version: fixture.version,
    api: fixture.api,
    target: fixture.target,
    author: fixture.author,
    homepage: fixture.homepage,
    license: fixture.license,
    source: fixture.source,
    package_sha256: fixture.package_sha256,
    permissions: fixture.permissions,
    operations: fixture.operations,
    is_upgrade,
    source_conflict,
    permission_expansions,
    newly_added_operations,
    existing_agent_visible_operations,
    requires_explicit_consent: true,
    requires_permission_consent: permission_expansions.length > 0,
  };
}

function buildEnabledMockModOperations(): OperationInfo[] {
  const ops: OperationInfo[] = [];
  for (const mod of mockInstalledMods) {
    if (!mod.enabled || mod.id === "text") continue;
    for (const summary of mod.operations) {
      ops.push({
        spec: {
          name: summary.name,
          description: summary.description,
          input_schema: summary.read_only
            ? {
                type: "object",
                properties: textDocumentProperties(),
                required: [],
                additionalProperties: false,
              }
            : {
                type: "object",
                properties: {
                  ...textDocumentProperties(),
                  style: { type: "string", default: "note" },
                },
                required: [],
                additionalProperties: false,
              },
          output_schema: summary.read_only
            ? {
                type: "object",
                properties: { issues: { type: "integer" } },
                required: ["issues"],
                additionalProperties: false,
              }
            : {
                type: "object",
                properties: textDocumentProperties(),
                required: [],
                additionalProperties: false,
              },
          annotations: {
            read_only: summary.read_only,
            destructive: summary.destructive,
            idempotent: summary.idempotent,
            deterministic: summary.deterministic,
            open_world: summary.open_world,
          },
        },
        provider: { mod: { id: mod.id } },
        health: { ...mod.health },
      });
    }
  }
  return ops;
}

/** Reset mock mod manager state (test helper). */
export function resetMockMods(): void {
  mockInstalledMods.length = 0;
  mockStagedLocalUpgrades.clear();
  resetMockModsM3();
}

/** Stage an upgraded local-directory manifest for `mockModsReloadLocal` / `mockModsPreviewDirectory` (test helper). */
export function stageMockLocalModUpgrade(
  modId = "markdown_tools",
  patch?: Partial<{ version: string; network: boolean; host_paths: boolean; addRemoteOp: boolean }>,
): void {
  mockStagedLocalUpgrades.set(modId, {
    version: patch?.version ?? "0.2.0",
    permissions: {
      host_paths: patch?.host_paths ?? false,
      events_subscribe: [],
      events_publish: [],
      recommended_agent_operations: ["markdown_tools.callout", "markdown_tools.export_remote"],
      network: patch?.network ?? true,
      filesystem_note: null,
    },
    operations: defaultThirdPartyOperations(patch?.addRemoteOp ?? true),
  });
}

function makePermissionDeniedError(modId: string, reasons: string[]) {
  return Object.assign(new Error("mod permission expansion requires user consent"), {
    code: "permission_denied",
    details: {
      kind: "permission_expansion",
      mod_id: modId,
      reasons,
    },
  });
}

export async function mockModsList(): Promise<ModListResponseDto> {
  await delay(10);
  const hasInstalledText = mockInstalledMods.some((entry) => entry.id === "text");
  const mods = hasInstalledText
    ? mockInstalledMods.map(cloneModEntry)
    : [bundledTextModEntry(), ...mockInstalledMods.map(cloneModEntry)];
  return {
    mods_root: MOCK_MODS_ROOT,
    mods,
  };
}

export async function mockModsOpenDir(): Promise<void> {
  await delay(10);
}

export async function mockModsPreviewArchive(input: {
  path: string;
}): Promise<ModInstallPreviewDto> {
  await delay(10);
  return buildPreviewFromFixture(input.path, "archive");
}

export async function mockModsPreviewDirectory(input: {
  path: string;
}): Promise<ModInstallPreviewDto> {
  await delay(10);
  return buildPreviewFromFixture(input.path, "directory");
}

function applyMockInstall(
  path: string,
  mode: "archive" | "directory",
  input: {
    consentGranted: boolean;
    consentPermissionExpansion: boolean;
    enabled: boolean;
    agentVisibleOperations: string[];
  },
): ModEntryDto {
  const preview = buildPreviewFromFixture(path, mode);
  if (preview.source_conflict) {
    throw Object.assign(new Error(preview.source_conflict), {
      code: "validation_failed",
    });
  }
  if (preview.requires_explicit_consent && !input.consentGranted) {
    throw Object.assign(new Error("installing third-party or local mod requires user consent"), {
      code: "permission_denied",
    });
  }
  if (preview.requires_permission_consent && !input.consentPermissionExpansion) {
    throw makePermissionDeniedError(preview.id, preview.permission_expansions);
  }
  const declared = new Set(preview.operations.map((op) => op.name));
  const agent_visible_operations = input.agentVisibleOperations.filter((name) =>
    declared.has(name),
  );
  const entry: ModEntryDto = {
    id: preview.id,
    name: preview.name,
    version: preview.version,
    api: preview.api,
    author: preview.author ?? null,
    homepage: preview.homepage ?? null,
    license: preview.license ?? null,
    entrypoint_kind: "binary",
    source: preview.source,
    install_dir: mode === "archive" ? `${MOCK_MODS_ROOT}/${preview.id}` : path,
    package_sha256: preview.package_sha256 ?? null,
    installed_at: new Date().toISOString(),
    enabled: input.enabled,
    read_only_bundled: false,
    health: { status: "available", diagnostic_code: null, message: null },
    sandbox: {
      backend: "seatbelt",
      enforced: true,
      network_allowed: preview.permissions.network,
      host_paths_allowed: preview.permissions.host_paths,
    },
    permissions: preview.permissions,
    operations: preview.operations,
    agent_visible_operations,
  };
  const existingIdx = mockInstalledMods.findIndex((item) => item.id === entry.id);
  if (existingIdx >= 0) {
    mockInstalledMods[existingIdx] = entry;
  } else {
    mockInstalledMods.push(entry);
  }
  mockStagedLocalUpgrades.delete(entry.id);
  emitEvent({
    type: "mod",
    kind: entry.enabled ? "mod.installed" : "mod.disabled",
    payload: { modId: entry.id },
  });
  return cloneModEntry(entry);
}

export async function mockModsInstallArchive(input: {
  path: string;
  consentGranted: boolean;
  consentPermissionExpansion: boolean;
  enabled: boolean;
  agentVisibleOperations: string[];
}): Promise<ModEntryDto> {
  await delay(15);
  return applyMockInstall(input.path, "archive", input);
}

export async function mockModsInstallDirectory(input: {
  path: string;
  consentGranted: boolean;
  consentPermissionExpansion: boolean;
  enabled: boolean;
  agentVisibleOperations: string[];
}): Promise<ModEntryDto> {
  await delay(15);
  return applyMockInstall(input.path, "directory", input);
}

export async function mockModsSetEnabled(input: {
  modId: string;
  enabled: boolean;
}): Promise<ModEntryDto> {
  await delay(10);
  const idx = mockInstalledMods.findIndex((item) => item.id === input.modId);
  if (idx < 0) {
    throw Object.assign(new Error(`mod '${input.modId}' not found`), { code: "not_found" });
  }
  mockInstalledMods[idx] = { ...mockInstalledMods[idx], enabled: input.enabled };
  emitEvent({
    type: "mod",
    kind: input.enabled ? "mod.enabled" : "mod.disabled",
    payload: { modId: input.modId },
  });
  return cloneModEntry(mockInstalledMods[idx]);
}

export async function mockModsSetAgentVisibility(input: {
  modId: string;
  operations: string[];
}): Promise<ModEntryDto> {
  await delay(10);
  const idx = mockInstalledMods.findIndex((item) => item.id === input.modId);
  if (idx < 0) {
    throw Object.assign(new Error(`mod '${input.modId}' not found`), { code: "not_found" });
  }
  const mod = mockInstalledMods[idx];
  const declared = new Set(mod.operations.map((op) => op.name));
  const filtered = input.operations.filter((name) => declared.has(name));
  mockInstalledMods[idx] = { ...mod, agent_visible_operations: filtered };
  emitEvent({
    type: "mod",
    kind: "mod.visibility_changed",
    payload: { modId: input.modId },
  });
  return cloneModEntry(mockInstalledMods[idx]);
}

export async function mockModsReloadLocal(input: {
  modId: string;
  consentPermissionExpansion: boolean;
}): Promise<ModEntryDto> {
  await delay(15);
  const idx = mockInstalledMods.findIndex((item) => item.id === input.modId);
  if (idx < 0) {
    throw Object.assign(new Error(`mod '${input.modId}' not found`), { code: "not_found" });
  }
  const existing = mockInstalledMods[idx];
  if (existing.source.kind !== "local_directory") {
    throw Object.assign(new Error(`mod '${input.modId}' is not mounted from a local directory`), {
      code: "validation_failed",
    });
  }
  const staged = mockStagedLocalUpgrades.get(input.modId);
  if (staged) {
    const expansions = computePermissionExpansions(existing.permissions, staged.permissions);
    if (expansions.length > 0 && !input.consentPermissionExpansion) {
      throw makePermissionDeniedError(input.modId, expansions);
    }
    const nextNames = new Set(staged.operations.map((op) => op.name));
    mockInstalledMods[idx] = {
      ...existing,
      version: staged.version,
      permissions: cloneModPermissions(staged.permissions),
      operations: staged.operations.map((op) => ({ ...op })),
      agent_visible_operations: existing.agent_visible_operations.filter((op) => nextNames.has(op)),
      installed_at: new Date().toISOString(),
    };
    mockStagedLocalUpgrades.delete(input.modId);
  }
  emitEvent({
    type: "mod",
    kind: "mod.reloaded",
    payload: { modId: input.modId },
  });
  return cloneModEntry(mockInstalledMods[idx]);
}

export async function mockModsUninstall(input: {
  modId: string;
  removeFiles: boolean;
}): Promise<void> {
  await delay(10);
  void input.removeFiles;
  const idx = mockInstalledMods.findIndex((item) => item.id === input.modId);
  if (idx < 0) {
    throw Object.assign(new Error(`mod '${input.modId}' not found`), { code: "not_found" });
  }
  mockInstalledMods.splice(idx, 1);
  mockStagedLocalUpgrades.delete(input.modId);
  emitEvent({
    type: "mod",
    kind: "mod.uninstalled",
    payload: { modId: input.modId },
  });
}

/* ───── Mod Manager (M3 Mocks) ───── */

const MOCK_OFFICIAL_CATALOG_URL = "https://github.com/Preference-cmd/Mera/releases/latest";

type MockCatalogEntrySpec = {
  source_id: string;
  source_kind: "official" | "third_party_url";
  source_label: string;
  id: string;
  name: string;
  version: string;
  description: string;
  api: string;
  target: string;
  author: string;
  license: string;
  entrypoint_kind: "binary" | "uv";
  capabilities: string[];
  permissions: ModPermissions;
  operations: ModOperationSummaryDto[];
  sha256: string;
  size_bytes: number;
};

const mockThirdPartySources: ThirdPartySourceDto[] = [];
const mockDownloadTasks = new Map<
  string,
  {
    snapshot: ModDownloadTaskDto;
    spec: MockCatalogEntrySpec;
    timer?: ReturnType<typeof setTimeout>;
  }
>();
let mockUvStatus: ModRuntimeEnvironmentDto = {
  sandbox_backend: "seatbelt",
  sandbox_enforced: true,
  uv: {
    available: false,
    source: "missing",
    path: null,
    managed_dir: `${MOCK_MODS_ROOT}/runtime/uv`,
  },
  uv_bootstrap_supported: true,
};
let mockDownloadDelayMs = 20;

export function setMockModDownloadDelay(ms: number): void {
  mockDownloadDelayMs = ms;
}

function defaultMockCatalogItems(): MockCatalogEntrySpec[] {
  const items: MockCatalogEntrySpec[] = [
    {
      source_id: "official",
      source_kind: "official",
      source_label: "Official Mera Registry",
      id: "json_tools",
      name: "JSON Tools",
      version: "0.1.0",
      description: "Query, transform, and validate structured JSON payloads.",
      api: "0.1",
      target: "noarch",
      author: "Mera Team",
      license: "GPL-3.0-or-later",
      entrypoint_kind: "binary",
      capabilities: ["json", "transform"],
      permissions: {
        host_paths: false,
        events_subscribe: [],
        events_publish: [],
        recommended_agent_operations: ["json_tools.query"],
        network: false,
        filesystem_note: null,
      },
      operations: [
        {
          name: "json_tools.query",
          description: "Extract fields from JSON documents.",
          read_only: true,
          destructive: false,
          idempotent: true,
          deterministic: true,
          open_world: false,
          uses_host_path: false,
          recommended_for_agent: true,
        },
      ],
      sha256: "c".repeat(64),
      size_bytes: 262144,
    },
  ];

  for (const src of mockThirdPartySources) {
    if (!src.enabled) continue;
    items.push({
      source_id: src.id,
      source_kind: "third_party_url",
      source_label: src.name,
      id: "markdown_tools",
      name: "Markdown Tools",
      version: "0.2.0",
      description: "Markdown callouts and link checking via community uv script.",
      api: "0.1",
      target: "noarch",
      author: "Community Author",
      license: "MIT",
      entrypoint_kind: "uv",
      capabilities: ["markdown", "lint"],
      permissions: {
        host_paths: false,
        events_subscribe: [],
        events_publish: [],
        recommended_agent_operations: ["markdown_tools.callout"],
        network: false,
        filesystem_note: null,
      },
      operations: defaultThirdPartyOperations(false),
      sha256: "d".repeat(64),
      size_bytes: 131072,
    });
  }
  return items;
}

export function resetMockModsM3(): void {
  mockThirdPartySources.length = 0;
  for (const entry of mockDownloadTasks.values()) {
    if (entry.timer) clearTimeout(entry.timer);
  }
  mockDownloadTasks.clear();
  mockDownloadDelayMs = 20;
  mockUvStatus = {
    sandbox_backend: "seatbelt",
    sandbox_enforced: true,
    uv: {
      available: false,
      source: "missing",
      path: null,
      managed_dir: `${MOCK_MODS_ROOT}/runtime/uv`,
    },
    uv_bootstrap_supported: true,
  };
}

export async function mockModsSourcesList(): Promise<ModSourcesListDto> {
  await delay(10);
  return {
    official_url: MOCK_OFFICIAL_CATALOG_URL,
    third_party: mockThirdPartySources.map((s) => ({ ...s })),
  };
}

export async function mockModsSourcesAdd(input: {
  name: string;
  url: string;
  publicKeyEd25519?: string | null;
}): Promise<ThirdPartySourceDto> {
  await delay(10);
  const name = input.name.trim();
  const url = input.url.trim();
  if (!name) {
    throw Object.assign(new Error("source name must not be empty"), { code: "command_failed" });
  }
  if (
    !url.startsWith("https://") &&
    !url.startsWith("http://127.0.0.1") &&
    !url.startsWith("http://localhost")
  ) {
    throw Object.assign(new Error("remote mod source must use https://"), {
      code: "command_failed",
    });
  }
  if (mockThirdPartySources.some((s) => s.url === url)) {
    throw Object.assign(new Error(`source '${url}' is already configured`), {
      code: "command_failed",
    });
  }
  const source: ThirdPartySourceDto = {
    id: `src-${name.toLowerCase().replace(/[^a-z0-9]+/g, "-") || "community"}`,
    name,
    url,
    enabled: true,
    has_public_key: Boolean(input.publicKeyEd25519?.trim()),
    public_key_ed25519: input.publicKeyEd25519?.trim() || null,
    added_at: new Date().toISOString(),
    last_refreshed_at: null,
    last_error: null,
  };
  mockThirdPartySources.push(source);
  return { ...source };
}

export async function mockModsSourcesSetEnabled(input: {
  sourceId: string;
  enabled: boolean;
}): Promise<ThirdPartySourceDto> {
  await delay(10);
  const found = mockThirdPartySources.find((s) => s.id === input.sourceId);
  if (!found) {
    throw Object.assign(new Error(`unknown source '${input.sourceId}'`), { code: "not_found" });
  }
  found.enabled = input.enabled;
  return { ...found };
}

export async function mockModsSourcesRemove(input: { sourceId: string }): Promise<void> {
  await delay(10);
  const idx = mockThirdPartySources.findIndex((s) => s.id === input.sourceId);
  if (idx >= 0) mockThirdPartySources.splice(idx, 1);
}

export async function mockModsBrowse(input: {
  sourceId?: string | null;
  forceRefresh?: boolean;
}): Promise<ModBrowseResponseDto> {
  await delay(15);
  void input.forceRefresh;
  const catalog = defaultMockCatalogItems().filter(
    (item) => !input.sourceId || item.source_id === input.sourceId,
  );
  const items: RemoteModCatalogItemDto[] = catalog.map((spec) => {
    const existing = mockInstalledMods.find((m) => m.id === spec.id);
    const sameOrigin =
      !existing ||
      (spec.source_kind === "official" && existing.source.kind === "official") ||
      (spec.source_kind === "third_party_url" &&
        existing.source.kind === "third_party_url" &&
        existing.source.source_url ===
          mockThirdPartySources.find((s) => s.id === spec.source_id)?.url);
    return {
      source_id: spec.source_id,
      source_kind: spec.source_kind,
      source_label: spec.source_label,
      id: spec.id,
      name: spec.name,
      version: spec.version,
      description: spec.description,
      api: spec.api,
      target: spec.target,
      author: spec.author,
      homepage: null,
      license: spec.license,
      permissions: cloneModPermissions(spec.permissions),
      capabilities: [...spec.capabilities],
      download_url: `https://example.invalid/${spec.id}-${spec.version}.rmod.tar.gz`,
      sha256: spec.sha256,
      size_bytes: spec.size_bytes,
      compatible: true,
      compatibility_reason: null,
      installed_version: existing?.version ?? null,
      update_available: Boolean(existing && existing.version !== spec.version),
      source_conflict:
        existing && !sameOrigin ? `already installed from '${existing.source.kind}'` : null,
    };
  });
  const outcomes = [
    { source_id: "official", source_name: "Official Mera Registry", ok: true, error: null },
    ...mockThirdPartySources
      .filter((s) => s.enabled)
      .map((s) => ({ source_id: s.id, source_name: s.name, ok: true, error: null })),
  ];
  return { items, outcomes };
}

function buildRemotePreview(spec: MockCatalogEntrySpec): ModInstallPreviewDto {
  const existing = mockInstalledMods.find((m) => m.id === spec.id);
  const srcObj: ModSourceDto =
    spec.source_kind === "official"
      ? { kind: "official", catalog_url: MOCK_OFFICIAL_CATALOG_URL }
      : {
          kind: "third_party_url",
          source_url:
            mockThirdPartySources.find((s) => s.id === spec.source_id)?.url ?? spec.source_label,
        };
  const sameOrigin =
    !existing ||
    (spec.source_kind === "official" && existing.source.kind === "official") ||
    (spec.source_kind === "third_party_url" &&
      existing.source.kind === "third_party_url" &&
      existing.source.source_url === (srcObj.kind === "third_party_url" ? srcObj.source_url : ""));
  const permission_expansions = computePermissionExpansions(
    existing?.permissions,
    spec.permissions,
  );
  const existingNames = new Set(existing?.operations.map((op) => op.name) ?? []);
  const newly_added_operations = existing
    ? spec.operations.filter((op) => !existingNames.has(op.name)).map((op) => op.name)
    : spec.operations.map((op) => op.name);
  const nextNames = new Set(spec.operations.map((op) => op.name));
  const existing_agent_visible_operations = existing
    ? existing.agent_visible_operations.filter((name) => nextNames.has(name))
    : [];

  return {
    id: spec.id,
    name: spec.name,
    version: spec.version,
    api: spec.api,
    target: spec.target,
    author: spec.author,
    homepage: null,
    license: spec.license,
    source: srcObj,
    package_sha256: spec.sha256,
    permissions: cloneModPermissions(spec.permissions),
    operations: spec.operations.map((op) => ({ ...op })),
    is_upgrade: Boolean(existing),
    source_conflict:
      existing && !sameOrigin ? `mod '${spec.id}' is already installed from another source` : null,
    permission_expansions,
    newly_added_operations,
    existing_agent_visible_operations,
    requires_explicit_consent: spec.source_kind !== "official",
    requires_permission_consent: permission_expansions.length > 0,
  };
}

function finalizeMockRemoteInstall(
  spec: MockCatalogEntrySpec,
  agentVisibleOperations: string[] | null,
): ModEntryDto {
  const srcObj: ModSourceDto =
    spec.source_kind === "official"
      ? { kind: "official", catalog_url: MOCK_OFFICIAL_CATALOG_URL }
      : {
          kind: "third_party_url",
          source_url:
            mockThirdPartySources.find((s) => s.id === spec.source_id)?.url ?? spec.source_label,
        };
  const existingIdx = mockInstalledMods.findIndex((m) => m.id === spec.id);
  const existing = existingIdx >= 0 ? mockInstalledMods[existingIdx] : undefined;
  const declared = new Set(spec.operations.map((op) => op.name));
  const visible =
    agentVisibleOperations !== null
      ? agentVisibleOperations.filter((n) => declared.has(n))
      : existing
        ? existing.agent_visible_operations.filter((n) => declared.has(n))
        : [];
  const entry: ModEntryDto = {
    id: spec.id,
    name: spec.name,
    version: spec.version,
    api: spec.api,
    author: spec.author,
    homepage: null,
    license: spec.license,
    entrypoint_kind: spec.entrypoint_kind,
    source: srcObj,
    install_dir: `${MOCK_MODS_ROOT}/packages/${spec.id}/${spec.version}`,
    package_sha256: spec.sha256,
    installed_at: new Date().toISOString(),
    enabled: true,
    read_only_bundled: false,
    health: { status: "available", diagnostic_code: null, message: null },
    sandbox: {
      backend: "seatbelt",
      enforced: true,
      network_allowed: spec.permissions.network,
      host_paths_allowed: spec.permissions.host_paths,
    },
    permissions: cloneModPermissions(spec.permissions),
    operations: spec.operations.map((op) => ({ ...op })),
    agent_visible_operations: visible,
  };
  if (existingIdx >= 0) {
    mockInstalledMods[existingIdx] = entry;
  } else {
    mockInstalledMods.push(entry);
  }
  emitEvent({
    type: "mod",
    kind: "mod.installed",
    payload: { modId: entry.id, version: entry.version },
  });
  return cloneModEntry(entry);
}

export async function mockModsDownloadStart(input: {
  sourceId: string;
  modId: string;
  version?: string | null;
}): Promise<ModDownloadTaskDto> {
  await delay(10);
  const spec = defaultMockCatalogItems().find(
    (item) =>
      item.source_id === input.sourceId &&
      item.id === input.modId &&
      (!input.version || item.version === input.version),
  );
  if (!spec) {
    throw Object.assign(new Error(`mod '${input.modId}' not found`), { code: "not_found" });
  }
  const taskId = rand("moddl");
  const snapshot: ModDownloadTaskDto = {
    task_id: taskId,
    source_id: spec.source_id,
    mod_id: spec.id,
    version: spec.version,
    status: "downloading",
    downloaded_bytes: 0,
    total_bytes: spec.size_bytes,
    error: null,
    preview: null,
  };
  const taskRecord: {
    snapshot: ModDownloadTaskDto;
    spec: MockCatalogEntrySpec;
    timer?: ReturnType<typeof setTimeout>;
  } = { snapshot, spec };
  mockDownloadTasks.set(taskId, taskRecord);

  emitEvent({
    type: "mod",
    kind: "mod.download_progress",
    payload: {
      taskId,
      sourceId: spec.source_id,
      modId: spec.id,
      version: spec.version,
      status: "downloading",
      downloadedBytes: 0,
      totalBytes: spec.size_bytes,
    },
  });

  taskRecord.timer = setTimeout(() => {
    const current = mockDownloadTasks.get(taskId);
    if (!current || current.snapshot.status === "cancelled") return;
    current.snapshot.downloaded_bytes = spec.size_bytes;
    const preview = buildRemotePreview(spec);
    if (
      preview.source_conflict ||
      preview.requires_explicit_consent ||
      preview.requires_permission_consent
    ) {
      current.snapshot.status = "awaiting_consent";
      current.snapshot.preview = preview;
      emitEvent({
        type: "mod",
        kind: "mod.download_progress",
        payload: {
          taskId,
          sourceId: spec.source_id,
          modId: spec.id,
          version: spec.version,
          status: "awaiting_consent",
          downloadedBytes: spec.size_bytes,
          totalBytes: spec.size_bytes,
        },
      });
    } else {
      finalizeMockRemoteInstall(spec, null);
      current.snapshot.status = "installed";
      emitEvent({
        type: "mod",
        kind: "mod.download_progress",
        payload: {
          taskId,
          sourceId: spec.source_id,
          modId: spec.id,
          version: spec.version,
          status: "installed",
          downloadedBytes: spec.size_bytes,
          totalBytes: spec.size_bytes,
        },
      });
    }
  }, mockDownloadDelayMs);

  return { ...snapshot };
}

export async function mockModsDownloadStatus(input: {
  taskId: string;
}): Promise<ModDownloadTaskDto> {
  await delay(5);
  const entry = mockDownloadTasks.get(input.taskId);
  if (!entry) {
    throw Object.assign(new Error(`unknown download task '${input.taskId}'`), {
      code: "not_found",
    });
  }
  return { ...entry.snapshot };
}

export async function mockModsDownloadCancel(input: {
  taskId: string;
}): Promise<ModDownloadTaskDto> {
  await delay(5);
  const entry = mockDownloadTasks.get(input.taskId);
  if (!entry) {
    throw Object.assign(new Error(`unknown download task '${input.taskId}'`), {
      code: "not_found",
    });
  }
  if (entry.timer) clearTimeout(entry.timer);
  entry.snapshot.status = "cancelled";
  entry.snapshot.preview = null;
  emitEvent({
    type: "mod",
    kind: "mod.download_progress",
    payload: {
      taskId: input.taskId,
      sourceId: entry.snapshot.source_id,
      modId: entry.snapshot.mod_id,
      version: entry.snapshot.version,
      status: "cancelled",
      downloadedBytes: entry.snapshot.downloaded_bytes,
      totalBytes: entry.snapshot.total_bytes,
    },
  });
  return { ...entry.snapshot };
}

export async function mockModsDownloadConfirmInstall(input: {
  taskId: string;
  consentPermissionExpansion: boolean;
  agentVisibleOperations: string[];
}): Promise<ModEntryDto> {
  await delay(10);
  void input.consentPermissionExpansion;
  const entry = mockDownloadTasks.get(input.taskId);
  if (!entry || entry.snapshot.status !== "awaiting_consent") {
    throw Object.assign(new Error(`download task '${input.taskId}' is not awaiting consent`), {
      code: "command_failed",
    });
  }
  const installed = finalizeMockRemoteInstall(entry.spec, input.agentVisibleOperations);
  entry.snapshot.status = "installed";
  entry.snapshot.preview = null;
  return installed;
}

export async function mockModsRuntimeStatus(): Promise<ModRuntimeEnvironmentDto> {
  await delay(5);
  return {
    ...mockUvStatus,
    uv: { ...mockUvStatus.uv },
  };
}

export async function mockModsUvBootstrap(): Promise<ModRuntimeEnvironmentDto> {
  await delay(20);
  mockUvStatus = {
    ...mockUvStatus,
    uv: {
      available: true,
      source: "managed_portable",
      path: `${MOCK_MODS_ROOT}/runtime/uv/uv`,
      managed_dir: `${MOCK_MODS_ROOT}/runtime/uv`,
    },
  };
  return {
    ...mockUvStatus,
    uv: { ...mockUvStatus.uv },
  };
}

/* ───── Storage Budget Mocks (S-d) ───── */

const MOCK_GIB = 1024 * 1024 * 1024;
const MOCK_MIB = 1024 * 1024;
const MOCK_LIBRARY_PREFLIGHT_THRESHOLD = 10 * MOCK_MIB;
const MOCK_FIFTY_GIB = 50 * MOCK_GIB;

type MockStorageState = {
  limitBytes: number | null;
  diskTotalBytes: number | null;
  diskAvailableBytes: number | null;
  diskFloorRatio: number;
  nudgeDismissed: boolean;
  scanRunning: boolean;
  modelsBytes: number;
  workersBytes: number;
  modsBytes: number;
  cacheBytes: number;
  projectHistoryBytes: Record<string, number>;
  projectProtectedHistoryBytes: Record<string, number>;
};

function defaultMockStorageState(): MockStorageState {
  return {
    limitBytes: null,
    diskTotalBytes: 500 * MOCK_GIB,
    diskAvailableBytes: 240 * MOCK_GIB,
    diskFloorRatio: 0.1,
    nudgeDismissed: false,
    scanRunning: false,
    modelsBytes: 9_010_000_000,
    workersBytes: 180 * MOCK_MIB,
    modsBytes: 48 * MOCK_MIB,
    cacheBytes: 256 * MOCK_MIB,
    projectHistoryBytes: {},
    projectProtectedHistoryBytes: {},
  };
}

let mockStorageState: MockStorageState = defaultMockStorageState();

const mockPathSizes: Record<string, number> = {};

export function resetMockStorage(overrides?: Partial<MockStorageState>): void {
  const base = defaultMockStorageState();
  mockStorageState = {
    ...base,
    ...overrides,
    projectHistoryBytes: { ...(overrides?.projectHistoryBytes ?? base.projectHistoryBytes) },
    projectProtectedHistoryBytes: {
      ...(overrides?.projectProtectedHistoryBytes ?? base.projectProtectedHistoryBytes),
    },
  };
  for (const key of Object.keys(mockPathSizes)) {
    delete mockPathSizes[key];
  }
}

export function setMockStoragePathSize(path: string, bytes: number): void {
  mockPathSizes[path] = bytes;
}

function buildMockStorageStatus(): StorageStatusDto {
  const activeHashes = new Set<string>();
  let libraryBytes = 0;
  for (const item of mockLibraryItems) {
    if (!activeHashes.has(item.hash)) {
      activeHashes.add(item.hash);
      libraryBytes += item.size;
    }
  }
  const activeOnlyHashes = new Set(
    mockLibraryItems.filter((item) => !item.removed).map((item) => item.hash),
  );
  const removedHashes = new Set<string>();
  let libraryRemovedBytes = 0;
  for (const item of mockLibraryItems) {
    if (item.removed && !activeOnlyHashes.has(item.hash) && !removedHashes.has(item.hash)) {
      removedHashes.add(item.hash);
      libraryRemovedBytes += item.size;
    }
  }

  const modelsBytes = mockStorageState.modelsBytes;

  const projectsMap = mockReadProjects();
  const projects = Object.values(projectsMap).map((project) => {
    const projectAssetBytes = Object.values(mockAssets)
      .filter((a) => a.projectId === project.id)
      .reduce((sum, a) => sum + Math.max(1024, a.content.length), 0);
    const assetsBytes = projectAssetBytes + 64 * 1024;
    const historyBytes = mockStorageState.projectHistoryBytes[project.id] ?? 0;
    return {
      projectId: project.id,
      name: project.name,
      assetsBytes,
      historyBytes,
      totalBytes: assetsBytes + historyBytes,
    };
  });
  const projectsTotalBytes = projects.reduce((sum, p) => sum + p.totalBytes, 0);
  const historyTotalBytes = projects.reduce((sum, p) => sum + p.historyBytes, 0);

  const totalBytes =
    libraryBytes +
    modelsBytes +
    mockStorageState.workersBytes +
    mockStorageState.modsBytes +
    mockStorageState.cacheBytes +
    projectsTotalBytes;

  const diskFloorBytes =
    mockStorageState.diskTotalBytes !== null
      ? Math.round(mockStorageState.diskTotalBytes * mockStorageState.diskFloorRatio)
      : null;

  const overBudget =
    mockStorageState.limitBytes !== null && totalBytes > mockStorageState.limitBytes;
  const belowDiskFloor =
    mockStorageState.diskAvailableBytes !== null &&
    diskFloorBytes !== null &&
    mockStorageState.diskAvailableBytes < diskFloorBytes;

  const showFiftyGbNudge =
    mockStorageState.limitBytes === null &&
    !mockStorageState.nudgeDismissed &&
    totalBytes > MOCK_FIFTY_GIB;

  return {
    totalBytes,
    limitBytes: mockStorageState.limitBytes,
    diskTotalBytes: mockStorageState.diskTotalBytes,
    diskAvailableBytes: mockStorageState.diskAvailableBytes,
    diskFloorRatio: mockStorageState.diskFloorRatio,
    diskFloorBytes,
    overBudget,
    belowDiskFloor,
    showFiftyGbNudge,
    nudgeDismissed: mockStorageState.nudgeDismissed,
    scanRunning: mockStorageState.scanRunning,
    categories: {
      libraryBytes,
      libraryRemovedBytes,
      modelsBytes,
      workersBytes: mockStorageState.workersBytes,
      modsBytes: mockStorageState.modsBytes,
      cacheBytes: mockStorageState.cacheBytes,
      projectsTotalBytes,
      historyTotalBytes,
    },
    projects,
  };
}

export async function mockStorageStatus(): Promise<StorageStatusDto> {
  await delay(10);
  return buildMockStorageStatus();
}

export async function mockStorageSetBudget(input: {
  limitBytes: number | null;
}): Promise<StorageStatusDto> {
  await delay(10);
  mockStorageState.limitBytes = input.limitBytes;
  // Mirror backend automatic cache recovery toward 90% target when over budget or below floor.
  let current = buildMockStorageStatus();
  if ((current.overBudget || current.belowDiskFloor) && mockStorageState.cacheBytes > 0) {
    const budgetTarget =
      input.limitBytes !== null ? Math.floor(input.limitBytes * 0.9) : current.totalBytes;
    const neededForBudget = Math.max(0, current.totalBytes - budgetTarget);
    const neededForFloor =
      current.belowDiskFloor &&
      current.diskFloorBytes !== null &&
      current.diskAvailableBytes !== null
        ? Math.max(0, current.diskFloorBytes - current.diskAvailableBytes)
        : 0;
    const reclaim = Math.min(
      mockStorageState.cacheBytes,
      Math.max(neededForBudget, neededForFloor),
    );
    mockStorageState.cacheBytes -= reclaim;
    if (mockStorageState.diskAvailableBytes !== null) {
      mockStorageState.diskAvailableBytes += reclaim;
    }
    current = buildMockStorageStatus();
  }
  return current;
}

export async function mockStorageDismissNudge(): Promise<StorageStatusDto> {
  await delay(10);
  mockStorageState.nudgeDismissed = true;
  return buildMockStorageStatus();
}

export async function mockStorageCleanCache(): Promise<StorageCleanCacheResult> {
  await delay(10);
  const freedBytes = mockStorageState.cacheBytes;
  mockStorageState.cacheBytes = 0;
  if (mockStorageState.diskAvailableBytes !== null) {
    mockStorageState.diskAvailableBytes += freedBytes;
  }
  return {
    freedBytes,
    status: buildMockStorageStatus(),
  };
}

export async function mockStorageCleanProjectHistory(input: {
  projectId: string;
}): Promise<StorageCleanCacheResult> {
  await delay(10);
  const current = mockStorageState.projectHistoryBytes[input.projectId] ?? 0;
  const protectedBytes = Math.min(
    current,
    mockStorageState.projectProtectedHistoryBytes[input.projectId] ?? 0,
  );
  const freedBytes = Math.max(0, current - protectedBytes);
  mockStorageState.projectHistoryBytes[input.projectId] = protectedBytes;
  if (mockStorageState.diskAvailableBytes !== null) {
    mockStorageState.diskAvailableBytes += freedBytes;
  }
  return {
    freedBytes,
    status: buildMockStorageStatus(),
  };
}

export async function mockStoragePreflightWrite(
  input: StoragePreflightWriteInput,
): Promise<StoragePreflightWriteResult> {
  await delay(5);
  const status = buildMockStorageStatus();
  const pathBytes = input.paths?.reduce((sum, p) => sum + (mockPathSizes[p] ?? 0), 0) ?? 0;
  const effectiveBytes = input.estimatedBytes > 0 ? input.estimatedBytes : pathBytes;
  const projectedTotalBytes = status.totalBytes + effectiveBytes;

  if (input.kind === "library" && effectiveBytes <= MOCK_LIBRARY_PREFLIGHT_THRESHOLD) {
    return {
      requiresConfirm: false,
      reason: null,
      projectedTotalBytes,
      limitBytes: status.limitBytes,
      availableBytes: status.diskAvailableBytes,
    };
  }

  let reason: StoragePreflightWriteResult["reason"] = null;
  if (
    status.belowDiskFloor ||
    (status.diskAvailableBytes !== null &&
      status.diskFloorBytes !== null &&
      status.diskAvailableBytes - effectiveBytes < status.diskFloorBytes)
  ) {
    reason = "below_disk_floor";
  } else if (status.overBudget) {
    reason = "over_budget";
  } else if (status.limitBytes !== null && projectedTotalBytes > status.limitBytes) {
    reason = "would_exceed_budget";
  }

  return {
    requiresConfirm: reason !== null,
    reason,
    projectedTotalBytes,
    limitBytes: status.limitBytes,
    availableBytes: status.diskAvailableBytes,
  };
}
