// @vitest-environment jsdom
import { act, StrictMode, type ReactNode } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { Run } from '../panels/Run';
import { Tasks } from '../panels/Tasks';
import { TaskSession } from '../panels/TaskSession';
import { isTaskSessionSnapshot, taskApi, type TaskReplayResult, type TaskSessionSnapshot } from '../tasks';
import { useStudio } from '../store';
import rustSnapshot from './task-status.json';
import { SkillOverview } from '../components/SkillOverview';

const catalog = [{
  key: { name: 'billing', version: '1' }, description: 'Review an invoice.',
  input_schema: { type: 'object' }, output_schema: { type: 'object' },
  tools: [{ name: 'refund', description: 'Issue the approved refund.', parameters: { type: 'object' }, effect: { kind: 'commit', idempotency_argument: 'operation_id' } }],
}];
const wireTask = { id: 'task-1', parent: null, skill: catalog[0]?.key, revision: 1, status: 'running', flow: null, pending: 'operation-1', output: null, services_pending: false, service_errors: [] };
const wireOperation = {
  id: 'operation-1', owner: { task: 'task-1', revision: 1, operation: 'operation-1' },
  tool: 'refund', args: { invoice: 'invoice-42', amount: 50, operation_id: 'refund-42' },
  idempotency_key: 'refund-42', status: 'awaiting_approval', cancellation_requested: false, result: null, error: null,
};
function status(overrides: Record<string, unknown> = {}): TaskSessionSnapshot {
  const value = { foreground: null, skills: catalog, tasks: [], operations: [], ...overrides };
  if (!isTaskSessionSnapshot(value)) throw new Error('Invalid test snapshot');
  return value;
}
function reply(snapshot = status()): TaskReplayResult {
  return { valid: true, errors: [], status: snapshot, snapshots: [{ ...snapshot, index: 0, event: 'initial', failures: [] }] };
}

class Socket {
  static OPEN = 1;
  static instances: Socket[] = [];
  readyState = 0;
  onopen: (() => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  send = vi.fn();
  close = vi.fn(() => { this.readyState = 3; });
  constructor() { Socket.instances.push(this); }
  open() { this.readyState = Socket.OPEN; this.onopen?.(); }
  receive(message: unknown) { this.onmessage?.({ data: JSON.stringify(message) }); }
}

let root: Root | undefined;
let container: HTMLDivElement;
async function mount(node: ReactNode) {
  root = createRoot(container);
  await act(async () => root?.render(node));
}
function button(text: string) {
  const found = [...container.querySelectorAll('button')].find((candidate) => candidate.textContent === text);
  if (!found) throw new Error(`Missing button: ${text}`);
  return found;
}
async function click(text: string) { await act(async () => button(text).click()); }
function latestSocket() {
  const socket = Socket.instances.at(-1);
  if (!socket) throw new Error('No socket');
  return socket;
}

beforeEach(() => {
  vi.stubGlobal('IS_REACT_ACT_ENVIRONMENT', true);
  vi.stubGlobal('WebSocket', Socket);
  Socket.instances = [];
  Object.defineProperty(HTMLElement.prototype, 'scrollTo', { configurable: true, value: vi.fn() });
  container = document.createElement('div');
  document.body.append(container);
  useStudio.getState().load({ name: 'skills-demo', skills: [{ name: 'billing', version: '1' }] });
});
afterEach(async () => {
  await act(async () => root?.unmount());
  root = undefined;
  container.remove();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe('task snapshot boundary', () => {
  it('accepts the snapshot serialized by the Rust task runtime', () => {
    expect(isTaskSessionSnapshot(rustSnapshot)).toBe(true);
  });
  it('requires ownership, lifecycle, catalog contracts, and operation outcomes', () => {
    const snapshot = status({ foreground: 'task-1', tasks: [wireTask], operations: [wireOperation] });
    expect(isTaskSessionSnapshot(snapshot)).toBe(true);
    for (const key of ['foreground', 'skills', 'tasks', 'operations']) {
      expect(isTaskSessionSnapshot(Object.fromEntries(Object.entries(snapshot).filter(([name]) => name !== key)))).toBe(false);
    }
    expect(isTaskSessionSnapshot({ ...snapshot, operations: [{ ...wireOperation, owner: null }] })).toBe(false);
    expect(isTaskSessionSnapshot({ ...snapshot, operations: [{ ...wireOperation, status: 'probably_done' }] })).toBe(false);
    expect(isTaskSessionSnapshot({ ...snapshot, tasks: [{ ...wireTask, revision: -1 }] })).toBe(false);
  });

  it('validates both the final snapshot and every HTTP replay snapshot', async () => {
    vi.stubGlobal('fetch', vi.fn()
      .mockResolvedValueOnce(new Response(JSON.stringify(reply())))
      .mockResolvedValueOnce(new Response(JSON.stringify({ ...reply(), snapshots: [{ event: 'initial' }] })))
      .mockResolvedValueOnce(new Response(JSON.stringify({ ...reply(), status: {} }))));
    await expect(taskApi.replay({}, [])).resolves.toEqual(reply());
    await expect(taskApi.replay({}, [])).rejects.toThrow('incompatible task status');
    await expect(taskApi.replay({}, [])).rejects.toThrow('incompatible task status');
  });
});

describe('skill authoring overview', () => {
  it('selects an independent definition and adds a versioned skill without creating a root flow', async () => {
    await mount(<SkillOverview />);
    const skill = container.querySelector('.skill-library button');
    if (!(skill instanceof HTMLButtonElement)) throw new Error('Missing skill');
    await act(async () => skill.click());
    expect(useStudio.getState().selection).toEqual({ kind: 'skill', index: 0 });
    await click('Add skill');
    expect(useStudio.getState().selection).toEqual({ kind: 'skill', index: 1 });
    expect(useStudio.getState().spec.skills).toEqual([
      { name: 'billing', version: '1' },
      { name: 'skill-2', version: '1', description: '', instruction: '' },
    ]);
    expect(useStudio.getState().spec.flow).toBeUndefined();
    await click('Try tasks offline');
    expect(useStudio.getState().dock).toBe('tasks');
  });
});

describe('task controls', () => {
  it('selects a currently admitted tool and disables tools blocked by the task dialogue', async () => {
    const skill = catalog[0];
    if (!skill) throw new Error('Missing test skill');
    await mount(<TaskSession status={status({
      foreground: 'task-1',
      skills: [{ ...skill, tools: [{ name: 'verify_account', description: 'Check account', parameters: null, effect: { kind: 'read' } }, ...skill.tools] }],
      tasks: [{ ...wireTask, pending: null, flow: { active: ['adjust'], done: ['verify'], allowed_tools: ['refund'], blocked_tools: { verify_account: 'Verification is complete' }, missing_requirements: ['done'], complete: false, overlay_path: [], terminated: false } }],
    })} disabled={false} onCommand={() => {}} />);
    const select = container.querySelector('select[aria-label="Tool"]');
    if (!(select instanceof HTMLSelectElement)) throw new Error('Missing tool select');
    expect(select.value).toBe('refund');
    expect([...select.options].find((option) => option.value === 'verify_account')?.disabled).toBe(true);
  });

  it('shows exact approval arguments and sends decisions for the owned operation', async () => {
    const onCommand = vi.fn();
    await mount(<TaskSession status={status({ foreground: 'task-1', tasks: [wireTask], operations: [wireOperation] })} disabled={false} onCommand={onCommand} />);
    expect(container.textContent).toContain('invoice-42');
    expect(container.textContent).toContain('refund-42');
    await click('Approve');
    expect(onCommand).toHaveBeenLastCalledWith({ action: 'decide', operation: 'operation-1', approve: true });
    await click('Deny');
    expect(onCommand).toHaveBeenLastCalledWith({ action: 'decide', operation: 'operation-1', approve: false });
    await click('Apply input revision');
    expect(onCommand).toHaveBeenLastCalledWith({ action: 'revise', task: 'task-1', expected_revision: 1, input: {} });
  });

  it('keeps an unknown commit visible after cancellation without claiming success', async () => {
    await mount(<TaskSession status={status({
      tasks: [{ ...wireTask, status: 'cancelled', pending: null }],
      operations: [{ ...wireOperation, status: 'unknown', cancellation_requested: true, error: 'Connection lost after dispatch' }],
    })} disabled={false} onCommand={() => {}} />);
    expect(container.textContent).toContain('Outcome unknown');
    expect(container.textContent).toContain('An external effect may already have occurred');
    expect(container.textContent).toContain('Connection lost after dispatch');
    expect(container.textContent).not.toContain('succeeded');
    expect([...container.querySelectorAll('button')].some((candidate) => candidate.textContent === 'Approve')).toBe(false);
  });

  it('waits for owned service results before approval or completion and shows service errors', async () => {
    await mount(<TaskSession status={status({
      foreground: 'task-1',
      tasks: [{ ...wireTask, services_pending: true, service_errors: ['Extraction unavailable'] }],
      operations: [wireOperation],
    })} disabled={false} onCommand={() => {}} />);
    expect(container.textContent).toContain('Updating task context');
    expect(container.textContent).toContain('Extraction unavailable');
    expect(button('Approve').disabled).toBe(true);
    expect(button('Complete task').disabled).toBe(true);
    expect(button('Cancel task').disabled).toBe(false);
  });
});

describe('offline task replay', () => {
  it('replays accumulated commands and retains tasks across dock navigation', async () => {
    const replay = vi.spyOn(taskApi, 'replay').mockResolvedValueOnce(reply()).mockResolvedValue(reply(status({ foreground: 'task-1', tasks: [{ ...wireTask, pending: null }] })));
    await mount(<Tasks active />);
    await click('Start task');
    expect(replay).toHaveBeenLastCalledWith(useStudio.getState().spec, [{ event: 'command', command: { action: 'start', skill: 'billing', input: {}, parent: null }, defer: false }]);
    await act(async () => root?.render(<Tasks active={false} />));
    await act(async () => root?.render(<Tasks active />));
    expect(replay).toHaveBeenCalledTimes(2);
    expect(container.textContent).toContain('task-1');
    await click('Suspend');
    expect(replay.mock.lastCall?.[1]).toEqual([
      { event: 'command', command: { action: 'start', skill: 'billing', input: {}, parent: null }, defer: false },
      { event: 'command', command: { action: 'suspend', task: 'task-1' }, defer: false },
    ]);
  });

  it('rejects an older document response and initializes the new revision', async () => {
    let resolveOld: (value: TaskReplayResult) => void = () => { throw new Error('Uninitialized request'); };
    const old = new Promise<TaskReplayResult>((resolve) => { resolveOld = resolve; });
    const replay = vi.spyOn(taskApi, 'replay').mockReturnValueOnce(old).mockResolvedValue(reply());
    await mount(<Tasks active />);
    await act(async () => useStudio.getState().edit({ name: 'new-document', skills: [{ name: 'billing', version: '2' }] }));
    await act(async () => resolveOld(reply(status({ foreground: 'task-1', tasks: [wireTask] }))));
    expect(replay).toHaveBeenCalledTimes(2);
    expect(container.textContent).not.toContain('task-1');
    expect(container.textContent).not.toContain('Replaying task commands');
  });

  it('initializes correctly during React strict effect replay', async () => {
    vi.spyOn(taskApi, 'replay').mockResolvedValue(reply());
    await mount(<StrictMode><Tasks active /></StrictMode>);
    expect(button('Start task').disabled).toBe(false);
    expect(container.textContent).not.toContain('Replaying task commands');
  });
});

describe('live task commands', () => {
  it('sends trusted commands on the current socket and disables them for an edited document', async () => {
    await mount(<Run />);
    await click('Start session');
    const socket = latestSocket();
    await act(async () => { socket.open(); socket.receive({ type: 'connected' }); socket.receive({ type: 'tasksStatus', status: status({ foreground: 'task-1', tasks: [wireTask], operations: [wireOperation] }) }); });
    await click('Approve');
    expect(socket.send).toHaveBeenLastCalledWith(JSON.stringify({ type: 'taskCommand', command: { action: 'decide', operation: 'operation-1', approve: true } }));
    expect(button('Approve').disabled).toBe(true);
    await act(async () => {
      useStudio.getState().edit({ name: 'changed', skills: [] });
      socket.receive({ type: 'tasksStatus', status: status({ foreground: 'task-1', tasks: [wireTask], operations: [wireOperation] }) });
    });
    expect(button('Approve').disabled).toBe(true);
    expect(container.textContent).toContain('Restart the session');
  });

  it('ignores task snapshots from a stopped socket and rejects malformed status', async () => {
    await mount(<Run />);
    await click('Start session');
    const old = latestSocket();
    await click('Stop');
    await click('Start session');
    const current = latestSocket();
    await act(async () => {
      current.open(); current.receive({ type: 'connected' });
      old.receive({ type: 'tasksStatus', status: status({ foreground: 'task-1', tasks: [wireTask] }) });
      current.receive({ type: 'tasksStatus', status: { foreground: null } });
    });
    expect(container.textContent).not.toContain('task-1');
    expect(container.textContent).toContain('invalid task status');
  });
});
