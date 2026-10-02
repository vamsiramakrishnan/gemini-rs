// @vitest-environment jsdom
import { act, type ReactNode } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { App } from '../App';
import { api, isFlowStatus, type FlowStatus, type TestRun, type Validation } from '../api';
import { Code } from '../panels/Code';
import { Preview } from '../panels/Preview';
import { Run } from '../panels/Run';
import { Tests } from '../panels/Tests';
import { canvasStatus, useStudio } from '../store';
import { liveConnected, sendPostures } from '../live';
import wireStatus from './flow-status.json';

// Keep the actual dock and panels; unrelated editor widgets do not need browser layout here.
vi.mock('@xyflow/react', () => ({ ReactFlowProvider: ({ children }: { children: ReactNode }) => children }));
vi.mock('../components/Toolbar', () => ({ Toolbar: () => null }));
vi.mock('../components/Outline', () => ({ Outline: () => null }));
vi.mock('../components/Inspector', () => ({ Inspector: () => null }));
vi.mock('../components/Canvas', () => ({ Canvas: () => null }));
vi.mock('../components/JsonEditor', () => ({ JsonEditor: () => null }));

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

const status: FlowStatus = {
  active: ['collect'], done: [], allowed_tools: ['lookup'], blocked_tools: {}, missing_requirements: ['finish'],
  complete: false, terminated: false, overlay_path: [],
};
const valid: Validation = { valid: true, errors: [], warnings: [] };
const passing: TestRun = { valid: true, errors: [], reports: [{ name: 'happy', passed: true, failures: [], events: 1 }], scenarios: [] };
let root: Root | undefined;
let container: HTMLDivElement;

function deferred<T>() {
  let resolve: (value: T) => void = () => { throw new Error('Promise is not initialized'); };
  let reject: (error: Error) => void = () => { throw new Error('Promise is not initialized'); };
  const promise = new Promise<T>((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}
async function mount(node: ReactNode) {
  root = createRoot(container);
  await act(async () => root?.render(node));
}
function button(text: string) {
  const found = [...container.querySelectorAll('button')].find((b) => b.textContent === text);
  if (!found) throw new Error(`Missing button: ${text}`);
  return found;
}
async function click(text: string) { await act(async () => button(text).click()); }
async function advance(ms: number) { await act(async () => { vi.advanceTimersByTime(ms); }); }
function latestSocket() {
  const socket = Socket.instances.at(-1);
  if (!socket) throw new Error('Session did not create a socket');
  return socket;
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal('IS_REACT_ACT_ENVIRONMENT', true);
  vi.stubGlobal('WebSocket', Socket);
  Object.defineProperty(HTMLElement.prototype, 'scrollTo', { configurable: true, value: vi.fn() });
  Socket.instances = [];
  container = document.createElement('div');
  document.body.append(container);
  useStudio.getState().load({ name: 'example', tests: [{ name: 'happy', events: [] }] });
  useStudio.setState({ dock: 'run', dockOpen: true, liveStatus: null, previewStatus: null });
  vi.spyOn(api, 'schema').mockResolvedValue({});
  vi.spyOn(api, 'validate').mockResolvedValue(valid);
});

afterEach(async () => {
  await act(async () => root?.unmount());
  root = undefined;
  container.remove();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  vi.clearAllTimers();
  vi.useRealTimers();
});

describe('workspace-owned live session', () => {
  it('keeps the socket and transcript across tabs and dock collapse, until Stop', async () => {
    await mount(<App />);
    await click('Start session');
    const socket = latestSocket();
    await act(async () => { socket.open(); socket.receive({ type: 'connected' }); socket.receive({ type: 'textDelta', text: 'Hello caller' }); });
    await click('Tests');
    expect(socket.close).not.toHaveBeenCalled();
    await click('Tests'); // Collapse the current dock.
    expect(socket.close).not.toHaveBeenCalled();
    await click('Run');
    expect(container.querySelector('.transcript')?.textContent).toContain('Hello caller');
    expect(liveConnected()).toBe(true);
    await click('Stop');
    expect(socket.send).toHaveBeenCalledWith(JSON.stringify({ type: 'stop' }));
    expect(socket.close).toHaveBeenCalledOnce();
    expect(liveConnected()).toBe(false);
  });

  it('ignores late callbacks from a stopped connection after a new session starts', async () => {
    await mount(<Run />);
    await click('Start session');
    const old = latestSocket();
    await click('Stop');
    await click('Start session');
    const current = latestSocket();
    await act(async () => { current.open(); current.receive({ type: 'connected' }); old.open(); old.onclose?.(); old.receive({ type: 'textDelta', text: 'stale' }); });
    expect(old.send).not.toHaveBeenCalled();
    expect(liveConnected()).toBe(true);
    expect(button('Stop')).toBeDefined();
    expect(container.textContent).not.toContain('stale');
    await click('Stop');
    expect(current.close).toHaveBeenCalledOnce();
  });

  it('keeps live status off a replay or edited graph and labels the running revision', async () => {
    await mount(<Run />);
    await click('Start session');
    const socket = latestSocket();
    await act(async () => { socket.open(); socket.receive({ type: 'connected' }); socket.receive({ type: 'flowStatus', status }); });
    expect(canvasStatus(useStudio.getState())?.active).toEqual(['collect']);
    const replay = { ...status, active: ['replay'] };
    await act(async () => {
      useStudio.getState().setPreviewStatus(replay, useStudio.getState().revision);
      useStudio.getState().openDock('preview');
      socket.receive({ type: 'flowStatus', status: { ...status, active: ['later-live'] } });
    });
    expect(canvasStatus(useStudio.getState())?.active).toEqual(['replay']);
    await act(async () => { useStudio.getState().edit({ name: 'changed' }); useStudio.getState().openDock('run'); socket.receive({ type: 'flowStatus', status }); });
    expect(canvasStatus(useStudio.getState())).toBeNull();
    expect(container.textContent).toContain('Restart the session');
    expect(socket.close).not.toHaveBeenCalled();
  });

  it('sends consecutive supported edits only along the connected document revision chain', async () => {
    useStudio.getState().load({ name: 'example', flow: { steps: [{ id: 'collect', posture: 'original' }] } });
    await mount(<Run />);
    await click('Start session');
    const socket = latestSocket();
    await act(async () => { socket.open(); socket.receive({ type: 'connected' }); });
    socket.send.mockClear();
    for (const posture of ['first edit', 'second edit']) {
      await act(async () => {
        const previousRevision = useStudio.getState().revision;
        useStudio.getState().edit({ name: 'example', flow: { steps: [{ id: 'collect', posture }] } });
        sendPostures({ postures: { collect: posture }, grounds: {}, previousRevision, revision: useStudio.getState().revision });
      });
    }
    expect(socket.send).toHaveBeenCalledTimes(2);
    expect(socket.send).toHaveBeenLastCalledWith(JSON.stringify({ type: 'updateFlowPostures', postures: { collect: 'second edit' }, grounds: {} }));
  });

  it.each(['edit', 'load'] as const)('blocks patches after a structural %s breaks the running revision chain', async (operation) => {
    await mount(<Run />);
    await click('Start session');
    const socket = latestSocket();
    await act(async () => { socket.open(); socket.receive({ type: 'connected' }); });
    socket.send.mockClear();
    await act(async () => {
      useStudio.getState()[operation]({ name: 'another document' });
      const previousRevision = useStudio.getState().revision;
      useStudio.getState().edit({ name: 'another document', flow: { steps: [{ id: 'collect', posture: 'new prompt' }] } });
      sendPostures({ postures: { collect: 'new prompt' }, grounds: {}, previousRevision, revision: useStudio.getState().revision });
    });
    expect(socket.send).not.toHaveBeenCalled();
  });
});

describe('runtime status wire contract', () => {
  it('accepts the real Rust snapshot fixture and rejects missing lifecycle or explanation fields', () => {
    expect(isFlowStatus(wireStatus)).toBe(true);
    for (const key of ['done', 'complete', 'terminated', 'overlay_path', 'active', 'allowed_tools', 'blocked_tools', 'missing_requirements']) {
      expect(isFlowStatus(Object.fromEntries(Object.entries(wireStatus).filter(([name]) => name !== key)))).toBe(false);
    }
    expect(isFlowStatus({ ...wireStatus, active_progress: { answer: { desc: 'waiting', holds: 'yes' } } })).toBe(false);
  });

  it('rejects incompatible replay snapshots at the HTTP boundary', async () => {
    const fetch = vi.fn()
      .mockResolvedValueOnce(new Response(JSON.stringify({ valid: true, errors: [], snapshots: [wireStatus] })))
      .mockResolvedValueOnce(new Response(JSON.stringify({ valid: true, errors: [], snapshots: [{ done: [] }] })));
    vi.stubGlobal('fetch', fetch);
    const replay = await api.simulate({}, 'test');
    expect(replay.snapshots).toEqual([wireStatus]);
    await expect(api.simulate({}, 'test')).rejects.toThrow('Restart the web server');
  });
});

describe('document-owned results', () => {
  it('invalidates validation and replay on edit, undo, redo and load, rejecting older validation', () => {
    const operations = [
      () => useStudio.getState().edit({ name: 'edited' }),
      () => useStudio.getState().undo(),
      () => useStudio.getState().redo(),
      () => useStudio.getState().load({ name: 'loaded' }),
    ];
    for (const operation of operations) {
      const revision = useStudio.getState().revision;
      useStudio.getState().setValidation(valid, revision);
      useStudio.getState().setPreviewStatus(status, revision);
      operation();
      expect(useStudio.getState().revision).toBeGreaterThan(revision);
      expect(useStudio.getState().validation).toBeNull();
      expect(useStudio.getState().previewStatus).toBeNull();
      useStudio.getState().setValidation(valid, revision);
      expect(useStudio.getState().validation).toBeNull();
    }
  });

  it('does not let a slow validation overwrite a newer document result', async () => {
    const old = deferred<Validation>();
    vi.mocked(api.validate).mockReturnValueOnce(old.promise).mockResolvedValue({ valid: false, errors: ['new error'], warnings: [] });
    await mount(<App />);
    await advance(350);
    await act(async () => useStudio.getState().edit({ name: 'new' }));
    await advance(350);
    await act(async () => old.resolve(valid));
    expect(useStudio.getState().validation?.errors).toEqual(['new error']);
  });

  it('hides old test passes on edits and rejects out-of-order results', async () => {
    const old = deferred<TestRun>();
    vi.spyOn(api, 'test').mockReturnValueOnce(old.promise).mockResolvedValue({ valid: false, errors: ['bad document'], reports: [], scenarios: [] });
    await mount(<Tests onPreview={() => {}} />);
    await click('Run tests');
    await act(async () => useStudio.getState().edit({ name: 'new' }));
    await click('Run tests');
    await act(async () => old.resolve(passing));
    expect(container.textContent).toContain('Invalid document');
    expect(container.textContent).not.toContain('0/0 passed');
    expect(container.textContent).not.toContain('1/1 passed');
    expect(container.querySelector('.ok')).toBeNull();
  });

  it('invalidates a finished test result and labels an empty suite without claiming a pass', async () => {
    vi.spyOn(api, 'test').mockResolvedValueOnce(passing).mockResolvedValue({ valid: true, errors: [], reports: [], scenarios: [] });
    await mount(<Tests onPreview={() => {}} />);
    await click('Run tests');
    expect(container.textContent).toContain('1/1 passed');
    await act(async () => useStudio.getState().edit({ name: 'new' }));
    expect(container.textContent).not.toContain('1/1 passed');
    await click('Run tests');
    expect(container.textContent).toContain('No tests to run');
    expect(container.querySelector('.ok')).toBeNull();
  });

  it('replays the selected test again after an edit and ignores its old rejected request', async () => {
    const old = deferred<Awaited<ReturnType<typeof api.simulate>>>();
    vi.spyOn(api, 'simulate').mockReturnValueOnce(old.promise).mockResolvedValue({ valid: true, errors: [], snapshots: [{ ...status, active: ['new-step'] }] });
    await mount(<Preview test="happy" onTest={() => {}} />);
    await advance(250);
    await act(async () => useStudio.getState().edit({ name: 'edited', tests: [{ name: 'happy', events: [] }] }));
    expect(container.querySelector('input[type=range]')).toBeNull();
    await advance(250);
    await act(async () => old.reject(new Error('old request failed')));
    expect(api.simulate).toHaveBeenCalledTimes(2);
    expect(container.textContent).toContain('new-step');
    expect(container.textContent).not.toContain('old request failed');
    expect(useStudio.getState().previewStatus?.value.active).toEqual(['new-step']);
  });

  it('disables downloads and removes old files when the document or language changes or generation fails', async () => {
    const python = deferred<Awaited<ReturnType<typeof api.project>>>();
    vi.spyOn(api, 'project')
      .mockResolvedValueOnce({ valid: true, errors: [], files: [{ path: 'main.rs', contents: 'old rust' }] })
      .mockReturnValueOnce(python.promise)
      .mockResolvedValue({ valid: false, errors: ['invalid project'], files: [] });
    await mount(<Code />);
    await advance(300);
    expect(button('Download .zip').disabled).toBe(false);
    await click('Python');
    expect(button('Download .zip').disabled).toBe(true);
    expect(container.textContent).not.toContain('old rust');
    await advance(300);
    await act(async () => useStudio.getState().edit({ name: 'changed' }));
    await advance(300);
    await act(async () => python.resolve({ valid: true, errors: [], files: [{ path: 'tools.py', contents: 'old python' }] }));
    expect(button('Download .zip').disabled).toBe(true);
    expect(container.textContent).toContain('invalid project');
    expect(container.textContent).not.toContain('old python');
  });
});
