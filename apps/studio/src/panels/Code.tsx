import { useEffect, useState } from 'react';
import JSZip from 'jszip';
import { api, type Language, type ProjectFile } from '../api';
import { useStudio } from '../store';

type Result = { revision: number; language: Language } & (
  | { kind: 'ready'; files: ProjectFile[]; name: string }
  | { kind: 'error'; message: string }
);

const LANGUAGES: { id: Language; label: string; blurb: string }[] = [
  { id: 'rust', label: 'Rust', blurb: 'A Cargo binary that runs the session, with the tools in process.' },
  { id: 'python', label: 'Python', blurb: 'An MCP tool server (official mcp package); agent.json binds the tools to it.' },
  { id: 'go', label: 'Go', blurb: 'An MCP tool server (official Go SDK); agent.json binds the tools to it.' },
];

/** The project `adk spec codegen` would write for this document. */
export function Code() {
  const spec = useStudio((s) => s.spec);
  const revision = useStudio((s) => s.revision);
  const [language, setLanguage] = useState<Language>('rust');
  const [result, setResult] = useState<Result | null>(null);
  const [open, setOpen] = useState<string>('');
  const currentResult = result?.revision === revision && result.language === language ? result : null;
  const project = currentResult?.kind === 'ready' ? currentResult : null;
  const files = project?.files ?? [];
  const error = currentResult?.kind === 'error' ? currentResult.message : null;

  useEffect(() => {
    let cancelled = false;
    const timer = setTimeout(() => {
      api
        .project(spec, language)
        .then((out) => {
          if (cancelled) return;
          if (!out.valid) return setResult({ revision, language, kind: 'error', message: out.errors.join('; ') });
          setResult({ revision, language, kind: 'ready', files: out.files, name: typeof spec.name === 'string' && spec.name ? spec.name : 'agent' });
          setOpen((current) => (out.files.some((f) => f.path === current) ? current : preferred(out.files)));
        })
        .catch((err: Error) => !cancelled && setResult({ revision, language, kind: 'error', message: err.message }));
    }, 300);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [spec, revision, language]);

  const download = async () => {
    if (!project || project.revision !== useStudio.getState().revision) return;
    const zip = new JSZip();
    const root = project.name;
    for (const file of project.files) zip.file(`${root}/${file.path}`, file.contents);
    const blob = await zip.generateAsync({ type: 'blob' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `${root}-${project.language}.zip`;
    a.click();
    URL.revokeObjectURL(url);
  };

  const current = files.find((f) => f.path === open);
  return (
    <div className="code">
      <div className="row">
        {LANGUAGES.map((l) => (
          <button key={l.id} type="button" className={language === l.id ? 'tab active' : 'tab'} onClick={() => setLanguage(l.id)}>
            {l.label}
          </button>
        ))}
        <span className="hint">{LANGUAGES.find((l) => l.id === language)!.blurb}</span>
        <button type="button" className="primary" onClick={download} disabled={!files.length}>
          Download .zip
        </button>
      </div>
      {error && <p className="error">{error}</p>}
      {!currentResult && <p className="hint">Generating project…</p>}
      <div className="code-body">
        <ul className="file-list">
          {files.map((file) => (
            <li key={file.path}>
              <button type="button" className={file.path === open ? 'active' : ''} onClick={() => setOpen(file.path)}>
                {file.path}
              </button>
            </li>
          ))}
        </ul>
        <pre className="file-view mono">{current?.contents ?? ''}</pre>
      </div>
      <p className="hint">
        Or from a terminal: <code>adk spec codegen agent.json --lang {language} --out my-agent</code>
      </p>
    </div>
  );
}

function preferred(files: ProjectFile[]): string {
  for (const path of ['src/tools.rs', 'tools.py', 'tools.go']) if (files.some((f) => f.path === path)) return path;
  return files[0]?.path ?? '';
}
