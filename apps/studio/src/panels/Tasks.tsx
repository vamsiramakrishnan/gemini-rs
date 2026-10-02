import { useCallback, useEffect, useRef, useState } from 'react';
import { useStudio } from '../store';
import { parseTaskInput, taskApi, type TaskReplayResult, type TaskReplayStep, type TaskSnapshot } from '../tasks';
import { isObject } from '../spec/paths';
import { TaskSession } from './TaskSession';

type Replay = { revision: number; commands: TaskReplayStep[] } & (
  | { kind: 'loading'; previous: TaskReplayResult | null }
  | { kind: 'ready'; result: TaskReplayResult }
  | { kind: 'error'; message: string }
);

/** Uses the runtime's controlled mock replay, retaining command history across dock navigation. */
export function Tasks({ active }: { active: boolean }) {
  const spec = useStudio((state) => state.spec);
  const revision = useStudio((state) => state.revision);
  const [replay, setReplay] = useState<Replay | null>(null);
  const [defer, setDefer] = useState(false);
  const sequence = useRef(0);
  const initialized = useRef<number | null>(null);
  const hasSkills = Array.isArray(spec.skills) && spec.skills.length > 0;
  const current = replay?.revision === revision ? replay : null;
  const busy = current?.kind === 'loading';
  const result = current?.kind === 'ready' ? current.result : current?.kind === 'loading' ? current.previous : null;

  const run = useCallback(async (commands: TaskReplayStep[]) => {
    const document = useStudio.getState();
    const request = ++sequence.current;
    initialized.current = document.revision;
    setReplay((previous) => ({
      kind: 'loading', revision: document.revision, commands,
      previous: previous?.revision === document.revision
        ? previous.kind === 'ready' ? previous.result : previous.kind === 'loading' ? previous.previous : null
        : null,
    }));
    try {
      const result = await taskApi.replay(document.spec, commands);
      if (sequence.current !== request || useStudio.getState().revision !== document.revision) return;
      setReplay({ kind: 'ready', revision: document.revision, commands, result });
    } catch (error) {
      if (sequence.current !== request || useStudio.getState().revision !== document.revision) return;
      setReplay({ kind: 'error', revision: document.revision, commands, message: error instanceof Error ? error.message : 'Task replay failed.' });
    }
  }, []);

  useEffect(() => {
    if (active && hasSkills && initialized.current !== revision) void run([]);
  }, [active, hasSkills, revision, run]);

  useEffect(() => () => { sequence.current += 1; initialized.current = null; }, []);

  const append = (step: TaskReplayStep) => {
    if (busy || !current || current.kind !== 'ready' || !current.result.valid) return;
    void run([...current.commands, step]);
  };
  const failures = result?.valid ? result.snapshots.flatMap((snapshot) => snapshot.failures.map((failure) => `Step ${snapshot.index}: ${failure}`)) : [];

  return <div className="task-preview">
    <div className="row">
      <strong>Task preview</strong>
      <span className="hint">Offline. Tools, extraction and memory use controlled fixtures; no provider session is started.</span>
      <button type="button" disabled={!hasSkills || busy} onClick={() => void run([])}>Reset tasks</button>
      <button type="button" disabled={!result?.valid || busy} onClick={() => append({ event: 'turn' })}>Advance turn</button>
    </div>
    {!hasSkills ? <p className="hint">Add reusable capabilities in the Skills section, then start tasks here.</p> : <>
      <label className="row"><input type="checkbox" checked={defer} onChange={(event) => setDefer(event.target.checked)} disabled={busy} />Hold new tool operations until I finish them</label>
      {busy && <p className="hint" role="status">Replaying task commands…</p>}
      {current?.kind === 'error' && <div className="row"><p className="error" role="alert">{current.message}</p><button type="button" onClick={() => void run(current.commands)}>Retry replay</button></div>}
      {result?.errors.map((error, index) => <p className="error" key={index}>{error}</p>)}
      {failures.map((failure, index) => <p className="error" key={index}>{failure}</p>)}
      {result?.valid && <TaskSession status={result.status} disabled={busy} onCommand={(command) => append({ event: 'command', command, defer })} onFinish={(operation) => append({ event: 'finish', operation })} />}
      {result?.valid && result.status.tasks.filter((task) => task.id === result.status.foreground).map((task) => <ObservedTurn key={`${task.id}:${task.revision}`} task={task} disabled={busy} onObserve={append} />)}
      {current && current.commands.length > 0 && <details className="task-history"><summary>{current.commands.length} replay steps</summary><pre>{JSON.stringify(current.commands, null, 2)}</pre></details>}
    </>}
  </div>;
}

function ObservedTurn({ task, disabled, onObserve }: { task: TaskSnapshot; disabled: boolean; onObserve: (step: TaskReplayStep) => void }) {
  const [user, setUser] = useState('');
  const [model, setModel] = useState('');
  const [extraction, setExtraction] = useState('{}');
  const [error, setError] = useState<string | null>(null);
  return <details className="task-history">
    <summary>Test a conversation turn</summary>
    <p className="hint">Run this task's extraction, computed fields, watchers and patterns. Supply each due extractor's result by its name, for example {`{"screening":{"is_spam":true}}`}.</p>
    <form className="task-input" onSubmit={(event) => {
      event.preventDefault();
      if (disabled) return;
      try {
        const values = parseTaskInput(extraction);
        if (!isObject(values)) throw new Error('Extraction results must be an object keyed by extractor name.');
        setError(null);
        onObserve({ event: 'observe', task: task.id, revision: task.revision, trigger: 'turn', user, model, extraction: values });
      } catch (error) {
        setError(error instanceof Error ? error.message : 'Enter valid extraction results.');
      }
    }}>
      <label>Caller transcript<textarea aria-label="Caller transcript" value={user} disabled={disabled} onChange={(event) => setUser(event.target.value)} /></label>
      <label>Agent transcript<textarea aria-label="Agent transcript" value={model} disabled={disabled} onChange={(event) => setModel(event.target.value)} /></label>
      <label>Controlled extraction results<textarea aria-label="Controlled extraction results" value={extraction} disabled={disabled} onChange={(event) => setExtraction(event.target.value)} spellCheck={false} /></label>
      {error && <p className="error" role="alert">{error}</p>}
      <button type="submit" disabled={disabled}>Apply test turn</button>
    </form>
  </details>;
}
