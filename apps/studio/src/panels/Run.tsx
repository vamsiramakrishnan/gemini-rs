import { useEffect, useRef, useState } from 'react';
import { useStudio } from '../store';
import { setLiveSocket } from '../live';
import { FlowState } from './FlowState';
import type { FlowStatus } from '../api';

interface Line {
  role: 'user' | 'model' | 'tool' | 'error' | 'notice';
  text: string;
}

/** A live session with the current document over `/ws/flow-studio`. The
 * server sandboxes the spec: HTTP and MCP bindings run as mocks unless the
 * operator allowed them. */
export function Run() {
  const spec = useStudio((s) => s.spec);
  const setStatus = useStudio((s) => s.setStatus);
  const [lines, setLines] = useState<Line[]>([]);
  const [connected, setConnected] = useState(false);
  const [status, setLocalStatus] = useState<FlowStatus | null>(null);
  const [text, setText] = useState('');
  const socket = useRef<WebSocket | null>(null);
  const streaming = useRef(false);
  const log = useRef<HTMLDivElement>(null);

  const append = (line: Line) => setLines((all) => [...all, line]);
  const appendModel = (delta: string, replace = false) =>
    setLines((all) => {
      const last = all[all.length - 1];
      if (streaming.current && last?.role === 'model') {
        return [...all.slice(0, -1), { role: 'model', text: replace ? delta : last.text + delta }];
      }
      streaming.current = true;
      return [...all, { role: 'model', text: delta }];
    });

  useEffect(() => {
    log.current?.scrollTo({ top: log.current.scrollHeight });
  }, [lines]);

  useEffect(() => () => stop(), []); // eslint-disable-line react-hooks/exhaustive-deps

  const start = () => {
    const proto = location.protocol === 'https:' ? 'wss' : 'ws';
    const ws = new WebSocket(`${proto}://${location.host}/ws/flow-studio`);
    socket.current = ws;
    setLines([{ role: 'notice', text: 'Connecting…' }]);
    ws.onopen = () => ws.send(JSON.stringify({ type: 'start', config: spec }));
    ws.onmessage = (event) => {
      if (typeof event.data !== 'string') return;
      const message = JSON.parse(event.data) as Record<string, unknown> & { type: string };
      switch (message.type) {
        case 'connected':
          setConnected(true);
          setLiveSocket(ws);
          append({ role: 'notice', text: 'Connected. The governed session is live.' });
          break;
        case 'textDelta':
          appendModel(String(message.text ?? ''));
          break;
        case 'textComplete':
          appendModel(String(message.text ?? ''), true);
          break;
        case 'outputTranscription':
          appendModel(String(message.text ?? ''));
          break;
        case 'turnComplete':
          streaming.current = false;
          break;
        case 'toolCallEvent':
          streaming.current = false;
          append({ role: 'tool', text: `${String(message.name)}(${short(String(message.args))}) → ${short(String(message.result))}` });
          break;
        case 'stateUpdate':
          if (message.key === 'studio:disabled_bindings' && Array.isArray(message.value)) {
            append({ role: 'notice', text: `Sandboxed: ${(message.value as string[]).join('; ')}` });
          }
          break;
        case 'flowStatus':
          setLocalStatus(message.status as FlowStatus);
          setStatus(message.status as FlowStatus);
          break;
        case 'error':
          append({ role: 'error', text: String(message.message ?? 'error') });
          break;
        default:
          break;
      }
    };
    ws.onclose = () => {
      setConnected(false);
      setLiveSocket(null);
      socket.current = null;
      append({ role: 'notice', text: 'Session ended.' });
    };
  };

  const stop = () => {
    const ws = socket.current;
    if (ws && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify({ type: 'stop' }));
    ws?.close();
    setStatus(null);
  };

  const send = () => {
    const line = text.trim();
    if (!line || !socket.current) return;
    streaming.current = false;
    append({ role: 'user', text: line });
    socket.current.send(JSON.stringify({ type: 'text', text: line }));
    setText('');
  };

  return (
    <div className="run">
      <div className="run-chat">
        <div className="row">
          <button type="button" className={connected ? 'danger' : 'primary'} onClick={() => (socket.current ? stop() : start())}>
            {socket.current ? 'Stop' : 'Start session'}
          </button>
          <span className="hint">Text session with this document. Postures and groundings you edit while it runs steer the next turn.</span>
        </div>
        <div className="transcript" ref={log}>
          {lines.map((line, i) => (
            <div key={i} className={`line line-${line.role}`}>
              {line.text}
            </div>
          ))}
        </div>
        <form
          className="row"
          onSubmit={(e) => {
            e.preventDefault();
            send();
          }}
        >
          <input type="text" value={text} placeholder={connected ? 'Say something…' : 'Start a session first'} disabled={!connected} onChange={(e) => setText(e.target.value)} />
          <button type="submit" disabled={!connected || !text.trim()}>
            Send
          </button>
        </form>
      </div>
      <div className="run-state">{status ? <FlowState status={status} /> : <p className="hint">The flow's state appears here once the session starts.</p>}</div>
    </div>
  );
}

function short(text: string, max = 160): string {
  return text.length > max ? `${text.slice(0, max)}…` : text;
}
