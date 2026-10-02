// The Studio's single source of truth: the spec document, the selection,
// undo history, and what the server last said about the document.

import { create } from 'zustand';
import type { FlowStatus, Validation } from './api';
import type { Spec } from './spec/graph';
import type { Path } from './spec/paths';
import type { RootSchema } from './spec/schema';

export type Selection =
  | { kind: 'node'; id: string }
  | { kind: 'edge'; id: string }
  | { kind: 'skill'; index: number }
  | { kind: 'section'; section: string }
  | null;

export type DockTab = 'problems' | 'tests' | 'preview' | 'tasks' | 'run' | 'code' | 'bundles';

const STORAGE_KEY = 'gemini-adk-studio:spec';
/** Edits to the same field within this window are one undo step. */
const COALESCE_MS = 800;
const HISTORY_LIMIT = 200;

export const BLANK_SPEC: Spec = {
  name: 'my-agent',
  instruction: 'You are a helpful voice agent.',
  conversation: {
    name: 'my-agent',
    stages: [
      { id: 'greet', say: 'Greet the caller and ask how you can help.', next: [{ to: 'done', when: 'always' }] },
      { id: 'done', terminal: true },
    ],
  },
};

export const BLANK_SKILL_SPEC: Spec = {
  name: 'my-assistant',
  instruction: 'You are a helpful voice assistant.',
  skills: [],
};

function restore(): Spec {
  try {
    const saved = localStorage.getItem(STORAGE_KEY);
    if (saved) return JSON.parse(saved) as Spec;
  } catch {
    // Storage unavailable or corrupt: start fresh.
  }
  return BLANK_SPEC;
}

interface StudioState {
  spec: Spec;
  revision: number;
  past: Spec[];
  future: Spec[];
  /** The field the last edit touched, and when, for coalescing. */
  lastEdit: { key: string; at: number } | null;
  selection: Selection;
  schema: RootSchema | null;
  validation: Validation | null;
  liveStatus: { revision: number; value: FlowStatus } | null;
  previewStatus: { revision: number; value: FlowStatus } | null;
  dock: DockTab;
  dockOpen: boolean;
  view: 'canvas' | 'json';
  /** The bundle this document was loaded from or last saved to. */
  bundle: { name: string; version?: string } | null;
  /** Bumped to ask the canvas to lay itself out again. */
  layoutEpoch: number;

  /** Replace the document. `key` names the field for undo coalescing;
   * omit it for structural edits, which are always their own step. */
  edit(spec: Spec, key?: string): void;
  /** Load a new document: clears history and selection. */
  load(spec: Spec, bundle?: { name: string; version?: string } | null): void;
  undo(): void;
  redo(): void;
  select(selection: Selection): void;
  setSchema(schema: RootSchema): void;
  setValidation(validation: Validation | null, revision: number): void;
  setLiveStatus(status: FlowStatus | null, revision: number): void;
  setPreviewStatus(status: FlowStatus | null, revision: number): void;
  openDock(tab: DockTab): void;
  toggleDock(): void;
  setView(view: 'canvas' | 'json'): void;
  setBundle(bundle: { name: string; version?: string } | null): void;
  relayout(): void;
}

export const useStudio = create<StudioState>((set, get) => ({
  spec: restore(),
  revision: 0,
  past: [],
  future: [],
  lastEdit: null,
  selection: null,
  schema: null,
  validation: null,
  liveStatus: null,
  previewStatus: null,
  dock: 'problems',
  dockOpen: true,
  view: 'canvas',
  bundle: null,
  layoutEpoch: 0,

  edit(spec, key) {
    const { spec: current, past, lastEdit } = get();
    if (spec === current) return;
    const now = Date.now();
    const coalesce = key !== undefined && lastEdit?.key === key && now - lastEdit.at < COALESCE_MS;
    set({
      spec,
      revision: get().revision + 1,
      validation: null,
      previewStatus: null,
      past: coalesce ? past : [...past, current].slice(-HISTORY_LIMIT),
      future: [],
      lastEdit: key !== undefined ? { key, at: now } : null,
    });
  },

  load(spec, bundle = null) {
    set({ spec, revision: get().revision + 1, validation: null, previewStatus: null, past: [], future: [], lastEdit: null, selection: null, bundle, layoutEpoch: get().layoutEpoch + 1 });
  },

  undo() {
    const { past, spec, future } = get();
    const previous = past[past.length - 1];
    if (!previous) return;
    set({ spec: previous, revision: get().revision + 1, validation: null, previewStatus: null, past: past.slice(0, -1), future: [spec, ...future], lastEdit: null });
  },

  redo() {
    const { past, spec, future } = get();
    const next = future[0];
    if (!next) return;
    set({ spec: next, revision: get().revision + 1, validation: null, previewStatus: null, past: [...past, spec], future: future.slice(1), lastEdit: null });
  },

  select: (selection) => set({ selection }),
  setSchema: (schema) => set({ schema }),
  setValidation: (validation, revision) => {
    if (get().revision === revision) set({ validation });
  },
  setLiveStatus: (value, revision) => set({ liveStatus: value ? { value, revision } : null }),
  setPreviewStatus: (value, revision) => {
    if (get().revision === revision) set({ previewStatus: value ? { value, revision } : null });
  },
  openDock: (dock) => set({ dock, dockOpen: true }),
  toggleDock: () => set({ dockOpen: !get().dockOpen }),
  setView: (view) => set({ view }),
  setBundle: (bundle) => set({ bundle }),
  relayout: () => set({ layoutEpoch: get().layoutEpoch + 1 }),
}));

/** Replay owns the canvas while its panel is selected; live events cannot replace it. */
export function canvasStatus(state: StudioState): FlowStatus | null {
  const result = state.dock === 'preview' ? state.previewStatus : state.liveStatus;
  return result?.revision === state.revision ? result.value : null;
}

// Keep the working document across reloads.
let saveTimer: ReturnType<typeof setTimeout> | undefined;
useStudio.subscribe((state, previous) => {
  if (state.spec === previous.spec) return;
  clearTimeout(saveTimer);
  saveTimer = setTimeout(() => {
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(state.spec));
    } catch {
      // Storage full or unavailable: the document still lives in memory.
    }
  }, 300);
});

/** A stable key for undo coalescing of edits at `path`. */
export function pathKey(path: Path): string {
  return path.join('/');
}
