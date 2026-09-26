import { useEffect, useRef, useState } from 'react';
import { api } from '../api';
import { BLANK_SPEC, useStudio } from '../store';
import { modeOf, toConversation, type Spec } from '../spec/graph';

export function Toolbar() {
  const spec = useStudio((s) => s.spec);
  const validation = useStudio((s) => s.validation);
  const past = useStudio((s) => s.past.length);
  const future = useStudio((s) => s.future.length);
  const view = useStudio((s) => s.view);
  const bundle = useStudio((s) => s.bundle);
  const { undo, redo, load, setView, edit, openDock } = useStudio.getState();
  const [gallery, setGallery] = useState<{ file: string; title?: string; name?: string }[]>([]);
  const fileInput = useRef<HTMLInputElement>(null);

  useEffect(() => {
    api.gallery().then(setGallery).catch(() => setGallery([]));
  }, []);

  const openFile = async (file: File) => {
    try {
      const parsed = JSON.parse(await file.text()) as Spec;
      // A bare conversation document is wrapped in a spec.
      if (Array.isArray(parsed.stages) && !parsed.conversation && !parsed.flow) {
        load({ name: typeof parsed.name === 'string' ? parsed.name : 'conversation', conversation: parsed });
      } else if (Array.isArray(parsed.steps) && !parsed.flow) {
        load({ name: 'flow', flow: parsed });
      } else {
        load(parsed);
      }
    } catch (err) {
      alert(`Could not open ${file.name}: ${(err as Error).message}`);
    }
  };

  const download = () => {
    const blob = new Blob([`${JSON.stringify(spec, null, 2)}\n`], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = 'agent.json';
    a.click();
    URL.revokeObjectURL(url);
  };

  const badge = !validation ? ['pending', '…'] : validation.valid ? ['valid', validation.warnings.length ? `valid · ${validation.warnings.length} warning${validation.warnings.length === 1 ? '' : 's'}` : 'valid'] : ['invalid', `${validation.errors.length} error${validation.errors.length === 1 ? '' : 's'}`];

  return (
    <header className="toolbar">
      <span className="brand">◈ Studio</span>
      <span className="doc-name">{typeof spec.name === 'string' ? spec.name : 'untitled'}</span>
      {bundle && (
        <span className="chip mono" title="Loaded from the bundle store">
          {bundle.name}
          {bundle.version ? `@${bundle.version.slice(0, 8)}` : ''}
        </span>
      )}
      <button type="button" className={`status status-${badge[0]}`} onClick={() => openDock('problems')}>
        {badge[1]}
      </button>
      <span className="spacer" />
      <button type="button" onClick={undo} disabled={!past} title="Undo (Ctrl+Z)">
        ↶
      </button>
      <button type="button" onClick={redo} disabled={!future} title="Redo (Ctrl+Shift+Z)">
        ↷
      </button>
      <div className="segmented">
        <button type="button" className={view === 'canvas' ? 'active' : ''} onClick={() => setView('canvas')}>
          Canvas
        </button>
        <button type="button" className={view === 'json' ? 'active' : ''} onClick={() => setView('json')}>
          JSON
        </button>
      </div>
      {modeOf(spec) === 'flow' && (
        <button type="button" onClick={() => edit(toConversation(spec))} title="Steps become stages; dependencies become guarded transitions">
          To conversation
        </button>
      )}
      <select
        value=""
        onChange={async (e) => {
          const choice = e.target.value;
          e.target.value = '';
          if (choice === '__new') load(BLANK_SPEC);
          else if (choice === '__open') fileInput.current?.click();
          else if (choice) load(await api.example(choice));
        }}
      >
        <option value="">File…</option>
        <option value="__new">New agent</option>
        <option value="__open">Open JSON…</option>
        {gallery.length > 0 && (
          <optgroup label="Examples">
            {gallery.map((g) => (
              <option key={g.file} value={g.file}>
                {g.title ?? g.name ?? g.file}
              </option>
            ))}
          </optgroup>
        )}
      </select>
      <button type="button" onClick={download}>
        Download
      </button>
      <button type="button" className="primary" onClick={() => openDock('bundles')} title="Save a version (Ctrl+S)">
        Save version
      </button>
      <input
        ref={fileInput}
        type="file"
        accept=".json,application/json"
        hidden
        onChange={(e) => {
          const file = e.target.files?.[0];
          if (file) void openFile(file);
          e.target.value = '';
        }}
      />
    </header>
  );
}
