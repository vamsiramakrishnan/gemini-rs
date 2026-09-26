import { useEffect, useState } from 'react';
import { ReactFlowProvider } from '@xyflow/react';
import { api } from './api';
import { useStudio, type DockTab } from './store';
import { Toolbar } from './components/Toolbar';
import { Outline } from './components/Outline';
import { Canvas } from './components/Canvas';
import { Inspector } from './components/Inspector';
import { JsonEditor } from './components/JsonEditor';
import { Problems } from './panels/Problems';
import { Tests } from './panels/Tests';
import { Preview } from './panels/Preview';
import { Run } from './panels/Run';
import { Code } from './panels/Code';
import { Bundles } from './panels/Bundles';

const TABS: { id: DockTab; label: string }[] = [
  { id: 'problems', label: 'Problems' },
  { id: 'tests', label: 'Tests' },
  { id: 'preview', label: 'Step through' },
  { id: 'run', label: 'Run' },
  { id: 'code', label: 'Code' },
  { id: 'bundles', label: 'Versions' },
];

export function App() {
  const view = useStudio((s) => s.view);
  const dock = useStudio((s) => s.dock);
  const dockOpen = useStudio((s) => s.dockOpen);
  const validation = useStudio((s) => s.validation);
  const [previewTest, setPreviewTest] = useState<string | null>(null);
  const [schemaError, setSchemaError] = useState<string | null>(null);

  useSchema(setSchemaError);
  useValidation();
  useShortcuts();

  const problems = validation ? validation.errors.length + validation.warnings.length : 0;
  return (
    <ReactFlowProvider>
      <div className="studio">
        <Toolbar />
        {schemaError && <div className="banner error">Could not load the spec schema: {schemaError}. Is the web server running?</div>}
        <div className="workspace">
          <Outline />
          <main className="center">{view === 'canvas' ? <Canvas /> : <JsonEditor />}</main>
          <Inspector />
        </div>
        <section className={`dock ${dockOpen ? 'open' : ''}`}>
          <div className="dock-tabs">
            {TABS.map((tab) => (
              <button
                key={tab.id}
                type="button"
                className={dock === tab.id && dockOpen ? 'active' : ''}
                onClick={() => (dock === tab.id && dockOpen ? useStudio.getState().toggleDock() : useStudio.getState().openDock(tab.id))}
              >
                {tab.label}
                {tab.id === 'problems' && problems > 0 && <span className={`count ${validation?.valid ? 'warn' : 'err'}`}>{problems}</span>}
              </button>
            ))}
          </div>
          {dockOpen && (
            <div className="dock-body">
              {dock === 'problems' && <Problems />}
              {dock === 'tests' && (
                <Tests
                  onPreview={(test) => {
                    setPreviewTest(test);
                    useStudio.getState().openDock('preview');
                  }}
                />
              )}
              {dock === 'preview' && <Preview test={previewTest} onTest={setPreviewTest} />}
              {dock === 'run' && <Run />}
              {dock === 'code' && <Code />}
              {dock === 'bundles' && <Bundles />}
            </div>
          )}
        </section>
      </div>
    </ReactFlowProvider>
  );
}

function useSchema(onError: (message: string | null) => void) {
  const setSchema = useStudio((s) => s.setSchema);
  useEffect(() => {
    api
      .schema()
      .then((schema) => {
        setSchema(schema);
        onError(null);
      })
      .catch((err: Error) => onError(err.message));
  }, [setSchema, onError]);
}

/** Validate as you edit, a moment after you stop typing. */
function useValidation() {
  const spec = useStudio((s) => s.spec);
  const setValidation = useStudio((s) => s.setValidation);
  useEffect(() => {
    let cancelled = false;
    const timer = setTimeout(() => {
      api
        .validate(spec)
        .then((result) => !cancelled && setValidation(result))
        .catch((err: Error) => !cancelled && setValidation({ valid: false, errors: [err.message], warnings: [] }));
    }, 350);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [spec, setValidation]);
}

function useShortcuts() {
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      const typing = target && (target.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(target.tagName) || target.closest('.cm-editor'));
      const mod = event.metaKey || event.ctrlKey;
      if (!mod) return;
      const key = event.key.toLowerCase();
      if (key === 's') {
        event.preventDefault();
        useStudio.getState().openDock('bundles');
      } else if (!typing && key === 'z') {
        event.preventDefault();
        if (event.shiftKey) useStudio.getState().redo();
        else useStudio.getState().undo();
      } else if (!typing && key === 'y') {
        event.preventDefault();
        useStudio.getState().redo();
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);
}
