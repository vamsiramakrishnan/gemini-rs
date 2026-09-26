import { useEffect, useState } from 'react';
import { api, type FlowStatus } from '../api';
import { useStudio } from '../store';
import { FlowState } from './FlowState';

/** Step through a flow test event by event; the canvas shows the flow's
 * state after each one. */
export function Preview({ test, onTest }: { test: string | null; onTest: (test: string | null) => void }) {
  const spec = useStudio((s) => s.spec);
  const setStatus = useStudio((s) => s.setStatus);
  const [snapshots, setSnapshots] = useState<FlowStatus[] | null>(null);
  const [index, setIndex] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const names = (Array.isArray(spec.tests) ? spec.tests : [])
    .map((t) => (t && typeof t === 'object' && !Array.isArray(t) && typeof t.name === 'string' ? t.name : null))
    .filter((n): n is string => n !== null);

  useEffect(() => {
    if (!test) {
      setSnapshots(null);
      return;
    }
    let cancelled = false;
    api
      .simulate(spec, test)
      .then((out) => {
        if (cancelled) return;
        if (!out.valid) {
          setError(out.errors.join('; '));
          setSnapshots(null);
        } else {
          setError(null);
          setSnapshots(out.snapshots);
          setIndex(0);
        }
      })
      .catch((err: Error) => setError(err.message));
    return () => {
      cancelled = true;
    };
    // Re-run when the test changes, not on every edit.
  }, [test]); // eslint-disable-line react-hooks/exhaustive-deps

  const snapshot = snapshots?.[index] ?? null;
  useEffect(() => {
    setStatus(snapshot);
    return () => setStatus(null);
  }, [snapshot, setStatus]);

  return (
    <div className="preview">
      <div className="row">
        <select value={test ?? ''} onChange={(e) => onTest(e.target.value || null)}>
          <option value="">Choose a flow test…</option>
          {names.map((name) => (
            <option key={name} value={name}>
              {name}
            </option>
          ))}
        </select>
        {snapshots && (
          <>
            <button type="button" onClick={() => setIndex(Math.max(0, index - 1))} disabled={index === 0}>
              ◀
            </button>
            <input type="range" min={0} max={snapshots.length - 1} value={index} onChange={(e) => setIndex(Number(e.target.value))} />
            <button type="button" onClick={() => setIndex(Math.min(snapshots.length - 1, index + 1))} disabled={index === snapshots.length - 1}>
              ▶
            </button>
            <span className="mono">
              {index}/{snapshots.length - 1}
            </span>
          </>
        )}
      </div>
      {error && <p className="error">{error}</p>}
      {!names.length && <p className="hint">This spec has no flow tests. Add one in the Flow tests section to step through it here.</p>}
      {snapshot && (
        <>
          <p className={snapshot.failures?.length ? 'error' : 'mono'}>
            {snapshot.event}
            {snapshot.failures?.length ? ` — ${snapshot.failures[0]}` : ''}
          </p>
          <FlowState status={snapshot} />
        </>
      )}
    </div>
  );
}
