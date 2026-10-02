// @vitest-environment jsdom
import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { Run } from '../panels/Run';
import { useStudio } from '../store';
import type { AudioEngine } from '../audio';

class Socket {
  static OPEN = 1;
  static instances: Socket[] = [];
  readyState = 0;
  binaryType = 'blob';
  onopen: (() => void) | null = null;
  onclose: (() => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;
  send = vi.fn();
  close = vi.fn(() => { this.readyState = 3; });
  constructor() { Socket.instances.push(this); }
  receive(message: unknown) { this.onmessage?.({ data: JSON.stringify(message) }); }
}

// Browser media devices are the boundary; the Run panel and audio lifecycle hook are real.
class MediaEngine implements AudioEngine {
  static instances: MediaEngine[] = [];
  onAudioData: ((data: string) => void) | null = null;
  onPlaybackDrained: (() => void) | null = null;
  initPlayback = vi.fn(async () => {});
  startRecording = vi.fn(async () => {});
  stopRecording = vi.fn();
  playAudio = vi.fn();
  playAudioBinary = vi.fn();
  clearQueue = vi.fn();
  destroy = vi.fn();
  constructor() { MediaEngine.instances.push(this); }
}

let root: Root;
let container: HTMLDivElement;
function button(text: string) {
  const result = [...container.querySelectorAll('button')].find((candidate) => candidate.textContent === text);
  if (!result) throw new Error(`Missing button ${text}`);
  return result;
}
async function click(text: string) { await act(async () => button(text).click()); }
function engine() {
  const result = MediaEngine.instances.at(-1);
  if (!result) throw new Error('Audio engine was not created');
  return result;
}
async function connect() {
  await click('Start session');
  const socket = Socket.instances.at(-1);
  if (!socket) throw new Error('Socket was not created');
  await act(async () => {
    socket.readyState = Socket.OPEN;
    socket.onopen?.();
    socket.receive({ type: 'connected' });
  });
  return socket;
}

beforeEach(async () => {
  vi.stubGlobal('IS_REACT_ACT_ENVIRONMENT', true);
  vi.stubGlobal('WebSocket', Socket);
  vi.stubGlobal('AudioManager', MediaEngine);
  Socket.instances = [];
  MediaEngine.instances = [];
  Object.defineProperty(HTMLElement.prototype, 'scrollTo', { configurable: true, value: vi.fn() });
  container = document.createElement('div');
  document.body.append(container);
  useStudio.getState().load({ name: 'voice' });
  root = createRoot(container);
  await act(async () => root.render(<Run />));
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe('Studio audio', () => {
  it('requests media only on click and uses the existing session for capture, playback and drain', async () => {
    const socket = await connect();
    expect(MediaEngine.instances).toHaveLength(0);
    expect(socket.binaryType).toBe('arraybuffer');
    await click('Enable audio');
    const media = engine();
    expect(media.startRecording).not.toHaveBeenCalled();
    await click('Use microphone');
    expect(media.startRecording).toHaveBeenCalledOnce();
    media.onAudioData?.('cGNt');
    expect(socket.send).toHaveBeenLastCalledWith(JSON.stringify({ type: 'audio', data: 'cGNt' }));
    const pcm = new ArrayBuffer(480);
    await act(async () => socket.onmessage?.({ data: pcm }));
    expect(media.playAudioBinary).toHaveBeenCalledWith(pcm);
    media.onPlaybackDrained?.();
    expect(socket.send).toHaveBeenLastCalledWith(JSON.stringify({ type: 'playbackDrained' }));
    await act(async () => socket.receive({ type: 'interrupted' }));
    expect(media.clearQueue).toHaveBeenCalledTimes(2);
    await click('Mute audio');
    media.playAudioBinary.mockClear();
    await act(async () => socket.onmessage?.({ data: pcm }));
    expect(media.playAudioBinary).not.toHaveBeenCalled();
    await click('Stop microphone');
    expect(media.stopRecording).toHaveBeenCalledOnce();
    expect(media.onAudioData).toBeNull();
  });

  it('releases media and ignores callbacks belonging to a stopped session', async () => {
    const old = await connect();
    await click('Use microphone');
    const media = engine();
    await click('Stop');
    expect(media.destroy).toHaveBeenCalledOnce();
    const current = await connect();
    const calls = old.send.mock.calls.length;
    media.onAudioData?.('stale');
    media.onPlaybackDrained?.();
    await act(async () => old.onmessage?.({ data: new ArrayBuffer(8) }));
    expect(old.send.mock.calls).toHaveLength(calls);
    expect(current.send).toHaveBeenCalledTimes(1);
    expect(media.playAudioBinary).not.toHaveBeenCalled();
    expect(button('Use microphone').disabled).toBe(false);
  });

  it('releases a late microphone grant without taking over a new session', async () => {
    await connect();
    await click('Enable audio');
    const oldMedia = engine();
    let resolve: () => void = () => { throw new Error('Uninitialized'); };
    oldMedia.startRecording.mockImplementationOnce(() => new Promise<void>((done) => { resolve = done; }));
    await click('Use microphone');
    expect(button('Waiting for microphone…').disabled).toBe(true);
    await click('Stop');
    const current = await connect();
    await click('Use microphone');
    const currentMedia = engine();
    const calls = current.send.mock.calls.length;
    await act(async () => resolve());
    expect(oldMedia.destroy).toHaveBeenCalledTimes(2);
    expect(currentMedia.destroy).not.toHaveBeenCalled();
    expect(button('Stop microphone').getAttribute('aria-pressed')).toBe('true');
    oldMedia.onAudioData?.('stale');
    expect(current.send.mock.calls).toHaveLength(calls);
  });

  it('reports permission denial, leaves text usable, and renders speech transcripts', async () => {
    const socket = await connect();
    await click('Enable audio');
    engine().startRecording.mockRejectedValueOnce(new Error('Permission denied'));
    await click('Use microphone');
    expect(container.textContent).toContain('Microphone unavailable: Permission denied');
    expect(container.querySelector('input')?.disabled).toBe(false);
    await act(async () => {
      socket.receive({ type: 'inputTranscription', text: 'Check ' });
      socket.receive({ type: 'inputTranscription', text: 'my invoice' });
      socket.receive({ type: 'outputTranscription', text: 'I can help.' });
    });
    expect(container.querySelector('.line-user')?.textContent).toBe('Check my invoice');
    expect(container.querySelector('.line-model')?.textContent).toBe('I can help.');
  });

  it('drains discarded playback once per turn and when the user mutes', async () => {
    const socket = await connect();
    socket.send.mockClear();
    await act(async () => {
      socket.onmessage?.({ data: new ArrayBuffer(480) });
      socket.onmessage?.({ data: new ArrayBuffer(480) });
    });
    expect(socket.send).not.toHaveBeenCalled();
    await act(async () => socket.receive({ type: 'turnComplete' }));
    expect(socket.send).toHaveBeenCalledExactlyOnceWith(JSON.stringify({ type: 'playbackDrained' }));
    await click('Enable audio');
    socket.send.mockClear();
    await act(async () => socket.receive({ type: 'turnComplete' }));
    expect(socket.send).not.toHaveBeenCalled();
    await click('Mute audio');
    expect(socket.send).toHaveBeenCalledExactlyOnceWith(JSON.stringify({ type: 'playbackDrained' }));
  });
});
