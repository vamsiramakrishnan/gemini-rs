import { useCallback, useEffect, useRef, useState } from 'react';
import { useStudio } from '../store';
import { setLiveSocket } from '../live';
import { FlowState } from './FlowState';
import { isFlowStatus, isRecord, type FlowStatus } from '../api';
import { isTaskSessionSnapshot, type TaskCommand, type TaskSessionSnapshot } from '../tasks';
import { TaskSession } from './TaskSession';
import { useSessionAudio } from '../audio';

interface Line {
  role: 'user' | 'model' | 'tool' | 'error' | 'notice';
  text: string;
}

type Connection = { kind: 'idle' } | { kind: 'connecting' | 'connected' | 'ended'; revision: number };

/** Mounted for the workspace lifetime: navigating the dock never ends a session. */
export function Run() {
  const revision = useStudio((s) => s.revision);
  const setLiveStatus = useStudio((s) => s.setLiveStatus);
  const [lines, setLines] = useState<Line[]>([]);
  const [connection, setConnection] = useState<Connection>({ kind: 'idle' });
  const [status, setLocalStatus] = useState<FlowStatus | null>(null);
  const [tasks, setTasks] = useState<TaskSessionSnapshot | null>(null);
  const [taskPending, setTaskPending] = useState(false);
  const [text, setText] = useState('');
  const socket = useRef<{ ws: WebSocket; revision: number } | null>(null);
  const streaming = useRef(false);
  const inputStreaming = useRef(false);
  const log = useRef<HTMLDivElement>(null);
  const connected = connection.kind === 'connected';
  const active = connection.kind === 'connecting' || connected;
  const changed = connection.kind !== 'idle' && connection.revision !== revision;

  const append = (line: Line) => setLines((all) => [...all, line]);
  const audio = useSessionAudio(socket, (message) => append({ role: 'error', text: message }));
  const stopAudio = audio.stop;
  const appendModel = (delta: string, replace = false) => {
    inputStreaming.current = false;
    const continuing = streaming.current;
    streaming.current = true;
    setLines((all) => {
      const last = all[all.length - 1];
      if (continuing && last?.role === 'model') {
        return [...all.slice(0, -1), { role: 'model', text: replace ? delta : last.text + delta }];
      }
      return [...all, { role: 'model', text: delta }];
    });
  };

  useEffect(() => {
    log.current?.scrollTo({ top: log.current.scrollHeight });
  }, [lines]);

  const stop = useCallback(() => {
    stopAudio();
    const current = socket.current;
    if (!current) return;
    socket.current = null;
    setLiveSocket(null);
    if (current.ws.readyState === WebSocket.OPEN) current.ws.send(JSON.stringify({ type: 'stop' }));
    current.ws.close();
    streaming.current = false;
    setTaskPending(false);
    setLiveStatus(null, current.revision);
    setConnection({ kind: 'ended', revision: current.revision });
    setLines((all) => [...all, { role: 'notice', text: 'Session ended.' }]);
  }, [setLiveStatus, stopAudio]);

  useEffect(() => () => {
    const current = socket.current;
    socket.current = null;
    current?.ws.close();
    setLiveSocket(null);
    if (current) setLiveStatus(null, current.revision);
  }, [setLiveStatus]);

  const start = () => {
    if (socket.current) return;
    const document = useStudio.getState();
    const startedRevision = document.revision;
    const proto = location.protocol === 'https:' ? 'wss' : 'ws';
    const ws = new WebSocket(`${proto}://${location.host}/ws/flow-studio`);
    ws.binaryType = 'arraybuffer';
    socket.current = { ws, revision: startedRevision };
    streaming.current = false;
    inputStreaming.current = false;
    setLocalStatus(null);
    setTasks(null);
    setTaskPending(false);
    setLiveStatus(null, startedRevision);
    setConnection({ kind: 'connecting', revision: startedRevision });
    setLines([{ role: 'notice', text: 'Connecting…' }]);
    ws.onopen = () => {
      if (socket.current?.ws === ws) ws.send(JSON.stringify({ type: 'start', config: document.spec }));
    };
    ws.onmessage = (event) => {
      if (socket.current?.ws !== ws || audio.receive(event.data) || typeof event.data !== 'string') return;
      let message: unknown;
      try {
        message = JSON.parse(event.data);
      } catch {
        append({ role: 'error', text: 'The server sent an invalid message.' });
        return;
      }
      if (!isRecord(message)) return;
      switch (message.type) {
        case 'connected':
          setConnection({ kind: 'connected', revision: startedRevision });
          setLiveSocket({ socket: ws, revision: startedRevision });
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
        case 'inputTranscription': {
          const continuing = inputStreaming.current;
          inputStreaming.current = true;
          streaming.current = false;
          setLines((all) => {
            const last = all.at(-1);
            const delta = String(message.text ?? '');
            return continuing && last?.role === 'user'
              ? [...all.slice(0, -1), { role: 'user', text: last.text + delta }]
              : [...all, { role: 'user', text: delta }];
          });
          break;
        }
        case 'audio':
          if (typeof message.data === 'string') audio.playBase64(message.data);
          break;
        case 'interrupted':
          audio.interrupt();
          streaming.current = false;
          break;
        case 'turnComplete':
          audio.turnComplete();
          streaming.current = false;
          inputStreaming.current = false;
          break;
        case 'toolCallEvent':
          streaming.current = false;
          append({ role: 'tool', text: `${String(message.name)}(${short(String(message.args))}) → ${short(String(message.result))}` });
          break;
        case 'stateUpdate':
          if (message.key === 'studio:disabled_bindings' && Array.isArray(message.value)) {
            append({ role: 'notice', text: `Sandboxed: ${message.value.map(String).join('; ')}` });
          }
          break;
        case 'flowStatus':
          if (isFlowStatus(message.status)) {
            setLocalStatus(message.status);
            setLiveStatus(message.status, startedRevision);
          } else {
            append({ role: 'error', text: 'The server sent an invalid flow status.' });
          }
          break;
        case 'tasksStatus':
          setTaskPending(false);
          if (isTaskSessionSnapshot(message.status)) {
            setTasks(message.status);
          } else {
            setTasks(null);
            append({ role: 'error', text: 'The server sent an invalid task status.' });
          }
          break;
        case 'error':
          setTaskPending(false);
          append({ role: 'error', text: String(message.message ?? 'error') });
          break;
        default:
          break;
      }
    };
    ws.onerror = () => {
      if (socket.current?.ws !== ws) return;
      append({ role: 'error', text: 'Could not communicate with the session. Start again to reconnect.' });
      stop();
    };
    ws.onclose = () => {
      if (socket.current?.ws !== ws) return;
      stopAudio();
      socket.current = null;
      setLiveSocket(null);
      setLiveStatus(null, startedRevision);
      setConnection({ kind: 'ended', revision: startedRevision });
      setTaskPending(false);
      append({ role: 'notice', text: 'Session ended.' });
    };
  };

  const send = () => {
    const line = text.trim();
    const ws = socket.current?.ws;
    if (!line || !connected || !ws || ws.readyState !== WebSocket.OPEN) return;
    streaming.current = false;
    inputStreaming.current = false;
    append({ role: 'user', text: line });
    ws.send(JSON.stringify({ type: 'text', text: line }));
    setText('');
  };

  const sendTask = (command: TaskCommand) => {
    const current = socket.current;
    if (!connected || taskPending || !current || current.ws.readyState !== WebSocket.OPEN || current.revision !== useStudio.getState().revision) return;
    current.ws.send(JSON.stringify({ type: 'taskCommand', command }));
    setTaskPending(true);
  };

  return (
    <div className={`run ${tasks ? 'run-tasks' : ''}`}>
      <div className="run-chat">
        <div className="row">
          <button type="button" className={active ? 'danger' : 'primary'} onClick={active ? stop : start}>
            {active ? 'Stop' : 'Start session'}
          </button>
          <button type="button" aria-pressed={audio.sound === 'on'} disabled={audio.sound === 'starting'} onClick={() => void audio.toggleSound()}>
            {audio.sound === 'starting' ? 'Starting audio…' : audio.sound === 'on' ? 'Mute audio' : 'Enable audio'}
          </button>
          <button type="button" aria-pressed={audio.microphone === 'on'} disabled={!connected || audio.microphone === 'starting' || audio.sound === 'starting'} onClick={() => void audio.toggleMicrophone()}>
            {audio.microphone === 'starting' ? 'Waiting for microphone…' : audio.microphone === 'on' ? 'Stop microphone' : 'Use microphone'}
          </button>
        </div>
        <p className="hint">Type below, or enable audio and use your microphone. Switching tabs keeps the session running; Stop ends it.</p>
        {changed && <p className="hint" role="status">Document changed. Restart the session to apply the complete current document. The transcript and session state below belong to the earlier revision.</p>}
        <div className="transcript" ref={log}>
          {lines.map((line, i) => <div key={i} className={`line line-${line.role}`}>{line.text}</div>)}
        </div>
        <form className="row" onSubmit={(e) => { e.preventDefault(); send(); }}>
          <input type="text" value={text} placeholder={connected ? 'Say something…' : 'Start a session first'} disabled={!connected} onChange={(e) => setText(e.target.value)} />
          <button type="submit" disabled={!connected || !text.trim()}>Send</button>
        </form>
      </div>
      <div className="run-state">
        {tasks ? <TaskSession status={tasks} disabled={!connected || changed || taskPending} onCommand={sendTask} /> : status ? <FlowState status={status} /> : <p className="hint">Session progress appears here once the session starts.</p>}
      </div>
    </div>
  );
}

function short(text: string, max = 160): string {
  return text.length > max ? `${text.slice(0, max)}…` : text;
}
