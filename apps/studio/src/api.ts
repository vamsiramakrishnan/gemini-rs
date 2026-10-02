// Typed client for the web server's Studio endpoints.

import type { Json, JsonObject } from './spec/paths';
import type { RootSchema } from './spec/schema';

export interface Validation {
  valid: boolean;
  errors: string[];
  warnings: string[];
  tools?: string[];
  steps?: number;
}

export interface TestStepResult {
  index: number;
  event: string;
  failures: string[];
}

export interface TestReport {
  name: string;
  passed: boolean;
  failures: TestStepResult[];
  events: number;
}

export interface ScenarioReport {
  name: string;
  passed: boolean;
  error?: string | null;
}

export interface TestRun {
  valid: boolean;
  errors: string[];
  reports: TestReport[];
  scenarios: ScenarioReport[];
}

/** A guard truth tree: which atom a step is waiting on. */
export interface Trace {
  desc: string;
  holds: boolean;
  children?: Trace[];
}

/** The governed flow's state, live (`flowStatus`) or replayed (preview). */
export interface FlowStatus {
  active: string[];
  done: string[];
  allowed_tools: string[];
  blocked_tools: Record<string, string>;
  missing_requirements: string[];
  active_progress?: Record<string, Trace>;
  complete: boolean;
  overlay_path: string[];
  terminated: boolean;
  event?: string;
  failures?: string[];
}

export function isFlowStatus(value: unknown): value is FlowStatus {
  if (!isRecord(value)) return false;
  return strings(value.done) && strings(value.overlay_path)
    && typeof value.complete === 'boolean' && typeof value.terminated === 'boolean'
    && strings(value.active) && strings(value.allowed_tools) && strings(value.missing_requirements)
    && (value.failures === undefined || strings(value.failures))
    && (value.event === undefined || typeof value.event === 'string')
    && isRecord(value.blocked_tools) && Object.values(value.blocked_tools).every((v) => typeof v === 'string')
    && (value.active_progress === undefined || (isRecord(value.active_progress) && Object.values(value.active_progress).every(isTrace)));
}

export function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function strings(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((v) => typeof v === 'string');
}

function isTrace(value: unknown): value is Trace {
  return isRecord(value) && typeof value.desc === 'string' && typeof value.holds === 'boolean'
    && (value.children === undefined || (Array.isArray(value.children) && value.children.every(isTrace)));
}

export interface ProjectFile {
  path: string;
  contents: string;
}

export type Language = 'rust' | 'python' | 'go';

export interface BundleVersion {
  name: string;
  version: string;
  digest: string;
  created_at: string;
  message?: string;
}

export interface BundleSummary {
  name: string;
  labels: Record<string, string>;
}

async function post<T>(url: string, body: unknown): Promise<T> {
  const response = await fetch(url, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
  return decode<T>(response);
}

async function decode<T>(response: Response): Promise<T> {
  const text = await response.text();
  let body: unknown;
  try {
    body = text ? JSON.parse(text) : {};
  } catch {
    throw new Error(`${response.status}: ${text.slice(0, 200)}`);
  }
  if (!response.ok) {
    const error = (body as { error?: string }).error ?? `${response.status} ${response.statusText}`;
    throw new Error(error);
  }
  return body as T;
}

export const api = {
  schema: async (): Promise<RootSchema> => decode(await fetch('/api/flows/schema')),
  validate: (spec: JsonObject) => post<Validation>('/api/flows/validate', spec),
  test: (spec: JsonObject) => post<TestRun>('/api/flows/test', spec),
  simulate: async (spec: JsonObject, test: string) => {
    const result = await post<{ valid: boolean; errors: string[]; snapshots: unknown }>('/api/flows/simulate', { spec, test });
    if (!Array.isArray(result.snapshots) || !result.snapshots.every(isFlowStatus)) {
      throw new Error('The server returned incompatible flow status. Restart the web server with the current SDK.');
    }
    return { ...result, snapshots: result.snapshots };
  },
  project: (spec: JsonObject, lang: Language) =>
    post<{ valid: boolean; errors: string[]; files: ProjectFile[] }>('/api/flows/project', { spec, lang }),

  gallery: async (): Promise<{ file: string; title?: string; name?: string; description?: string }[]> => {
    const index = await decode<{ examples: { file: string; title?: string; name?: string; description?: string }[] }>(
      await fetch('/static/examples/flows/index.json'),
    );
    return index.examples;
  },
  example: async (file: string): Promise<JsonObject> => decode(await fetch(`/static/examples/flows/${file}`)),

  bundles: async (): Promise<{ store: string; bundles: BundleSummary[] }> => decode(await fetch('/api/bundles')),
  bundle: async (name: string): Promise<{ versions: BundleVersion[]; labels: Record<string, string> }> =>
    decode(await fetch(`/api/bundles/${encodeURIComponent(name)}`)),
  load: async (reference: string): Promise<{ version: BundleVersion; spec: JsonObject }> =>
    decode(await fetch(`/api/bundle?ref=${encodeURIComponent(reference)}`)),
  push: (name: string, spec: JsonObject, message: string, labels: string[]) =>
    post<{ version: BundleVersion }>(`/api/bundles/${encodeURIComponent(name)}`, { spec, message, labels }),
  label: async (name: string, label: string, version: string): Promise<{ version: string }> =>
    decode(
      await fetch(`/api/bundles/${encodeURIComponent(name)}/labels/${encodeURIComponent(label)}`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ version }),
      }),
    ),
};

export type { Json };
