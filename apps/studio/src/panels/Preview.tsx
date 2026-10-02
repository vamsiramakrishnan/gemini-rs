import { useEffect, useState } from 'react';
import { api, type FlowStatus } from '../api';
import { useStudio } from '../store';
import { FlowState } from './FlowState';
import { isObject } from '../spec/paths';

type Result = { revision: number; test: string } & (
  | { kind: 'ready'; snapshots: FlowStatus[] }
  | { kind: 'error'; message: string }
);

/** Step through a flow test event by event; the canvas shows the flow's
 * state after each one. */
export function Preview({ test, onTest }: { test: string | null; onTest: (test: string | null) => void }) {
  const spec = useStudio((s) => s.spec);
  const revision = useStudio((s) => s.revision);
  const setStatus = useStudio((s) => s.setPreviewStatus);
  const [result, setResult] = useState<Result | null>(null);
  const [index, setIndex] = useState(0);
  const names = (Array.isArray(spec.tests) ? spec.tests : [])
    .map((t) => (t && typeof t === 'object' && !Array.isArray(t) && typeof t.name === 'string' ? t.name : null))
    .filter((n): n is string => n !== null);
  for (const skill of Array.isArray(spec.skills) ? spec.skills : []) {
    if (!isObject(skill) || typeof skill.name !== 'string') continue;
    for (const test of Array.isArray(skill.tests) ? skill.tests : []) {
      if (isObject(test) && typeof test.name === 'string') names.push(`${skill.name}/${test.name}`);
    }
  }
  const selected = test && names.includes(test) ? test : null;
  const current = result?.revision === revision && result.test === selected ? result : null;
  const snapshots = current?.kind === 'ready' ? current.snapshots : null;
  const error = current?.kind === 'error' ? current.message : null;

  useEffect(() => {
    if (!selected) {
      if (test) onTest(null);
      return;
    }
    let cancelled = false;
    const timer = setTimeout(() => api
      .simulate(spec, selected)
      .then((out) => {
        if (cancelled) return;
        if (!out.valid) {
          setResult({ revision, test: selected, kind: 'error', message: out.errors.join('; ') });
        } else {
          setResult({ revision, test: selected, kind: 'ready', snapshots: out.snapshots });
          setIndex(0);
        }
      })
      .catch((err: Error) => {
        if (!cancelled) setResult({ revision, test: selected, kind: 'error', message: err.message });
      }), 250);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [spec, revision, selected, test, onTest]);

  const snapshot = snapshots?.[index] ?? null;
  useEffect(() => {
    setStatus(snapshot, revision);
    return () => setStatus(null, revision);
  }, [snapshot, revision, setStatus]);

  return (
    <div className="preview">
      <div className="row">
        <select value={selected ?? ''} onChange={(e) => onTest(e.target.value || null)}>
          <option value="">Choose a flow test…</option>
          {names.map((name) => (
            <option key={name} value={name}>
              {name}
            </option>
          ))}
        </select>
        {snapshots && snapshots.length > 0 && (
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
      {selected && !current && <p className="hint">Updating preview…</p>}
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
