import { useStudio } from '../store';

/** Validation results for the current document, refreshed as you edit. */
export function Problems() {
  const validation = useStudio((s) => s.validation);
  if (!validation) return <p className="hint">Validating…</p>;
  const { errors, warnings } = validation;
  if (!errors.length && !warnings.length) return <p className="ok">No problems. The spec compiles, and every guard reads a key something writes.</p>;
  return (
    <ul className="problems">
      {errors.map((message, i) => (
        <li key={`e${i}`} className="error">
          <span className="sev">error</span> {message}
        </li>
      ))}
      {warnings.map((message, i) => (
        <li key={`w${i}`} className="warning">
          <span className="sev">warning</span> {message}
        </li>
      ))}
    </ul>
  );
}
