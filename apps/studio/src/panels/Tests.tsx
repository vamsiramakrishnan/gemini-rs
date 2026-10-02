import { useRef, useState } from 'react';
import { api, type TestRun } from '../api';
import { useStudio } from '../store';
import { isObject } from '../spec/paths';

type Result = { revision: number } & (
  | { kind: 'running' }
  | { kind: 'ready'; run: TestRun }
  | { kind: 'error'; message: string }
);

/** Run the spec's flow tests and conversation scenarios offline: scripted
 * events through the real flow monitor and simulator, no model involved. */
export function Tests({ onPreview }: { onPreview: (test: string) => void }) {
  const spec = useStudio((s) => s.spec);
  const revision = useStudio((s) => s.revision);
  const [result, setResult] = useState<Result | null>(null);
  const request = useRef(0);
  const current = result?.revision === revision ? result : null;
  const run = current?.kind === 'ready' ? current.run : null;
  const busy = current?.kind === 'running';
  const error = current?.kind === 'error' ? current.message : null;
  const skills = Array.isArray(spec.skills) ? spec.skills.filter(isObject) : [];
  const tests = (Array.isArray(spec.tests) ? spec.tests.length : 0)
    + skills.reduce((count, skill) => count + (Array.isArray(skill.tests) ? skill.tests.length : 0), 0);
  const scenarios = Array.isArray(spec.scenarios) ? spec.scenarios.length : 0;
  const taskScenarios = Array.isArray(spec.task_scenarios) ? spec.task_scenarios.length : 0;

  const go = async () => {
    const id = ++request.current;
    setResult({ revision, kind: 'running' });
    try {
      const run = await api.test(spec);
      if (id === request.current && revision === useStudio.getState().revision) setResult({ revision, kind: 'ready', run });
    } catch (err) {
      if (id === request.current && revision === useStudio.getState().revision) {
        setResult({ revision, kind: 'error', message: err instanceof Error ? err.message : String(err) });
      }
    }
  };

  const passed = run ? [...run.reports, ...run.scenarios].filter((r) => r.passed).length : 0;
  const total = run ? run.reports.length + run.scenarios.length : 0;
  return (
    <div className="tests">
      <div className="row">
        <button type="button" className="primary" onClick={go} disabled={busy}>
          {busy ? 'Running…' : 'Run tests'}
        </button>
        <span className="hint">
          {tests} flow test{tests === 1 ? '' : 's'}, {scenarios} conversation scenario{scenarios === 1 ? '' : 's'}, {taskScenarios} task scenario{taskScenarios === 1 ? '' : 's'}. Run tests executes all three.
        </span>
        {run?.valid && total > 0 && (
          <span className={passed === total ? 'ok' : 'error'}>
            {passed}/{total} passed
          </span>
        )}
        {run && !run.valid && <span className="error">Invalid document — tests did not run</span>}
        {run?.valid && total === 0 && <span className="hint">No tests to run</span>}
      </div>
      {error && <p className="error">{error}</p>}
      {run && !run.valid && (
        <ul className="problems">
          {run.errors.map((e, i) => (
            <li key={i} className="error">
              {e}
            </li>
          ))}
        </ul>
      )}
      {run && (
        <ul className="results">
          {run.reports.map((report) => (
            <li key={`t-${report.name}`} className={report.passed ? 'pass' : 'fail'}>
              <span className="mark">{report.passed ? '✓' : '✗'}</span> test <b>{report.name}</b>
              <button type="button" className="link" onClick={() => onPreview(report.name)}>
                step through
              </button>
              {report.failures.flatMap((step) =>
                step.failures.map((failure, i) => (
                  <div key={`${step.index}-${i}`} className="failure">
                    event {step.index} ({step.event}): {failure}
                  </div>
                )),
              )}
            </li>
          ))}
          {run.scenarios.map((report) => (
            <li key={`s-${report.name}`} className={report.passed ? 'pass' : 'fail'}>
              <span className="mark">{report.passed ? '✓' : '✗'}</span> scenario <b>{report.name}</b>
              {report.error && <div className="failure">{report.error}</div>}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
