import { useState } from 'react';
import { api, type TestRun } from '../api';
import { useStudio } from '../store';

/** Run the spec's flow tests and conversation scenarios offline: scripted
 * events through the real flow monitor and simulator, no model involved. */
export function Tests({ onPreview }: { onPreview: (test: string) => void }) {
  const spec = useStudio((s) => s.spec);
  const [run, setRun] = useState<TestRun | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const tests = Array.isArray(spec.tests) ? spec.tests.length : 0;
  const scenarios = Array.isArray(spec.scenarios) ? spec.scenarios.length : 0;

  const go = async () => {
    setBusy(true);
    setError(null);
    try {
      setRun(await api.test(spec));
    } catch (err) {
      setError((err as Error).message);
    } finally {
      setBusy(false);
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
          {tests} flow test{tests === 1 ? '' : 's'}, {scenarios} scenario{scenarios === 1 ? '' : 's'}. Add them in the Flow tests and Scenarios sections.
        </span>
        {run && (
          <span className={passed === total ? 'ok' : 'error'}>
            {passed}/{total} passed
          </span>
        )}
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
