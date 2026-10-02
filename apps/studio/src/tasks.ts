import { isFlowStatus, isRecord, type FlowStatus } from './api';
import type { Json, JsonObject } from './spec/paths';

export type TaskId = string & { readonly __brand: 'TaskId' };
export type OperationId = string & { readonly __brand: 'OperationId' };
export type TaskStatus = 'running' | 'suspended' | 'completed' | 'cancelled' | 'failed';
export type OperationStatus = 'awaiting_approval' | 'running' | 'succeeded' | 'failed' | 'declined' | 'cancelled' | 'unknown';
export type TaskToolEffect = { kind: 'read' } | { kind: 'commit'; idempotency_argument: string };

export interface SkillKey { name: string; version: string }
export interface TaskTool {
  name: string;
  description: string;
  parameters: Json;
  effect: TaskToolEffect;
}
export interface SkillSummary {
  key: SkillKey;
  description: string;
  input_schema: Json;
  output_schema: Json;
  tools: TaskTool[];
}
export interface TaskSnapshot {
  id: TaskId;
  parent: TaskId | null;
  skill: SkillKey;
  revision: number;
  status: TaskStatus;
  flow: FlowStatus | null;
  pending: OperationId | null;
  output: Json;
  services_pending: boolean;
  service_errors: string[];
}
export interface OperationSnapshot {
  id: OperationId;
  owner: { task: TaskId; revision: number; operation: OperationId };
  tool: string;
  args: Json;
  idempotency_key: string | null;
  status: OperationStatus;
  cancellation_requested: boolean;
  result: Json;
  error: string | null;
}
/** Mirrors the L1 task runtime observation, shared by live sessions and replay. */
export interface TaskSessionSnapshot {
  foreground: TaskId | null;
  skills: SkillSummary[];
  tasks: TaskSnapshot[];
  operations: OperationSnapshot[];
}
export type TaskCommand =
  | { action: 'start'; skill: string; input: Json; parent: TaskId | null }
  | { action: 'suspend' | 'resume' | 'cancel'; task: TaskId }
  | { action: 'revise'; task: TaskId; expected_revision: number; input: Json }
  | { action: 'complete'; task: TaskId; output: Json }
  | { action: 'invoke'; task: TaskId; tool: string; args: Json; idempotency_key: string | null }
  | { action: 'decide'; operation: OperationId; approve: boolean }
  | { action: 'reconcile'; operation: OperationId; outcome: { status: 'succeeded'; result: Json } | { status: 'failed'; error: string } };

export function isJson(value: unknown): value is Json {
  return value === null || typeof value === 'boolean' || typeof value === 'string'
    || (typeof value === 'number' && Number.isFinite(value))
    || (Array.isArray(value) && value.every(isJson))
    || (isRecord(value) && Object.values(value).every(isJson));
}

export function parseTaskInput(text: string): Json {
  const value: unknown = JSON.parse(text);
  if (!isJson(value)) throw new Error('Enter a JSON value.');
  return value;
}

function isTaskId(value: unknown): value is TaskId {
  return typeof value === 'string' && value.length > 0;
}
function isOperationId(value: unknown): value is OperationId {
  return typeof value === 'string' && value.length > 0;
}
function isRevision(value: unknown): value is number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0;
}
function isSkillKey(value: unknown): value is SkillKey {
  return isRecord(value) && typeof value.name === 'string' && typeof value.version === 'string';
}
function isTaskTool(value: unknown): value is TaskTool {
  if (!isRecord(value) || !isRecord(value.effect)) return false;
  return typeof value.name === 'string' && typeof value.description === 'string' && isJson(value.parameters)
    && (value.effect.kind === 'read' || (value.effect.kind === 'commit' && typeof value.effect.idempotency_argument === 'string'));
}
function isSkillSummary(value: unknown): value is SkillSummary {
  return isRecord(value) && isSkillKey(value.key) && typeof value.description === 'string'
    && isJson(value.input_schema) && isJson(value.output_schema)
    && Array.isArray(value.tools) && value.tools.every(isTaskTool);
}
function isTaskSnapshot(value: unknown): value is TaskSnapshot {
  return isRecord(value) && isTaskId(value.id) && (value.parent === null || isTaskId(value.parent))
    && isSkillKey(value.skill) && isRevision(value.revision)
    && (value.status === 'running' || value.status === 'suspended' || value.status === 'completed' || value.status === 'cancelled' || value.status === 'failed')
    && (value.flow === null || isFlowStatus(value.flow)) && (value.pending === null || isOperationId(value.pending))
    && isJson(value.output) && typeof value.services_pending === 'boolean'
    && isStrings(value.service_errors);
}
function isOperationSnapshot(value: unknown): value is OperationSnapshot {
  return isRecord(value) && isOperationId(value.id) && isRecord(value.owner) && isTaskId(value.owner.task)
    && isRevision(value.owner.revision) && isOperationId(value.owner.operation)
    && typeof value.tool === 'string' && isJson(value.args)
    && (value.idempotency_key === null || typeof value.idempotency_key === 'string')
    && (value.status === 'awaiting_approval' || value.status === 'running' || value.status === 'succeeded' || value.status === 'failed'
      || value.status === 'declined' || value.status === 'cancelled' || value.status === 'unknown')
    && typeof value.cancellation_requested === 'boolean' && isJson(value.result)
    && (value.error === null || typeof value.error === 'string');
}
export function isTaskSessionSnapshot(value: unknown): value is TaskSessionSnapshot {
  return isRecord(value) && (value.foreground === null || isTaskId(value.foreground))
    && Array.isArray(value.skills) && value.skills.every(isSkillSummary)
    && Array.isArray(value.tasks) && value.tasks.every(isTaskSnapshot)
    && Array.isArray(value.operations) && value.operations.every(isOperationSnapshot);
}

export type TaskReplayStep =
  | { event: 'command'; command: TaskCommand; defer?: boolean }
  | { event: 'turn' }
  | { event: 'observe'; task: TaskId; revision: number; trigger: 'turn'; user: string; model: string; extraction: JsonObject }
  | { event: 'finish'; operation: OperationId };
export interface TaskReplaySnapshot extends TaskSessionSnapshot {
  index: number;
  event: string;
  failures: string[];
}
export type TaskReplayResult =
  | { valid: false; errors: string[] }
  | { valid: true; errors: string[]; status: TaskSessionSnapshot; snapshots: TaskReplaySnapshot[] };

function isStrings(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((entry) => typeof entry === 'string');
}
function isTaskReplaySnapshot(value: unknown): value is TaskReplaySnapshot {
  return isTaskSessionSnapshot(value) && 'index' in value && isRevision(value.index)
    && 'event' in value && typeof value.event === 'string' && 'failures' in value && isStrings(value.failures);
}

export const taskApi = {
  async replay(spec: JsonObject, commands: TaskReplayStep[]): Promise<TaskReplayResult> {
    const response = await fetch('/api/flows/tasks', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ spec, commands }),
    });
    const result: unknown = await response.json();
    if (!response.ok) {
      throw new Error(isRecord(result) && typeof result.error === 'string' ? result.error : `Task replay failed (${response.status}).`);
    }
    if (isRecord(result) && isStrings(result.errors)) {
      if (result.valid === false) return { valid: false, errors: result.errors };
      if (result.valid === true && isTaskSessionSnapshot(result.status)
        && Array.isArray(result.snapshots) && result.snapshots.every(isTaskReplaySnapshot)) {
        return { valid: true, errors: result.errors, status: result.status, snapshots: result.snapshots };
      }
    }
    throw new Error('The server returned incompatible task status. Restart the web server with the current SDK.');
  },
};
