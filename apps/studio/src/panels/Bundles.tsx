import { useCallback, useEffect, useState } from 'react';
import { api, type BundleSummary, type BundleVersion } from '../api';
import { useStudio } from '../store';

function slug(name: unknown): string {
  return (typeof name === 'string' ? name : '')
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '');
}

/** Save the document as a bundle version, browse versions, and move labels
 * (promote to prod, roll back) — what a runtime loads by `name:label`. */
export function Bundles() {
  const spec = useStudio((s) => s.spec);
  const bundle = useStudio((s) => s.bundle);
  const setBundle = useStudio((s) => s.setBundle);
  const load = useStudio((s) => s.load);
  const [store, setStore] = useState('');
  const [all, setAll] = useState<BundleSummary[]>([]);
  const [name, setName] = useState(bundle?.name ?? slug(spec.name));
  const [versions, setVersions] = useState<BundleVersion[]>([]);
  const [labels, setLabels] = useState<Record<string, string>>({});
  const [message, setMessage] = useState('');
  const [label, setLabel] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const list = await api.bundles();
      setStore(list.store);
      setAll(list.bundles);
      if (name && list.bundles.some((b) => b.name === name)) {
        const detail = await api.bundle(name);
        setVersions([...detail.versions].reverse());
        setLabels(detail.labels);
      } else {
        setVersions([]);
        setLabels({});
      }
      setError(null);
    } catch (err) {
      setError((err as Error).message);
    }
  }, [name]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const save = async () => {
    try {
      const labelsToSet = label.trim() ? [label.trim()] : [];
      const { version } = await api.push(name, spec, message, labelsToSet);
      setBundle({ name, version: version.version });
      setNotice(`Saved ${name}@${version.version}${labelsToSet.length ? ` and pointed :${labelsToSet[0]} at it` : ''}.`);
      setMessage('');
      await refresh();
    } catch (err) {
      setError((err as Error).message);
    }
  };

  const promote = async (target: string, version: string) => {
    try {
      await api.label(name, target, version);
      setNotice(`${name}:${target} → ${version}`);
      await refresh();
    } catch (err) {
      setError((err as Error).message);
    }
  };

  const open = async (version: string) => {
    try {
      const out = await api.load(`${name}@${version}`);
      load(out.spec, { name, version: out.version.version });
      setNotice(`Opened ${name}@${out.version.version}.`);
    } catch (err) {
      setError((err as Error).message);
    }
  };

  const labelsOf = (version: string) => Object.entries(labels).filter(([, v]) => v === version).map(([l]) => l);

  return (
    <div className="bundles">
      <div className="row">
        <label>
          Bundle <input type="text" value={name} onChange={(e) => setName(slug(e.target.value) || e.target.value.toLowerCase())} list="bundle-names" />
        </label>
        <datalist id="bundle-names">
          {all.map((b) => (
            <option key={b.name} value={b.name} />
          ))}
        </datalist>
        <input type="text" placeholder="What changed?" value={message} onChange={(e) => setMessage(e.target.value)} />
        <input type="text" placeholder="label (optional)" value={label} onChange={(e) => setLabel(e.target.value)} list="bundle-labels" className="narrow" />
        <datalist id="bundle-labels">
          {['staging', 'prod', ...Object.keys(labels)].map((l) => (
            <option key={l} value={l} />
          ))}
        </datalist>
        <button type="button" className="primary" onClick={save} disabled={!name}>
          Save version
        </button>
        <span className="hint mono">{store}</span>
      </div>
      {notice && <p className="ok">{notice}</p>}
      {error && <p className="error">{error}</p>}
      {versions.length === 0 ? (
        <p className="hint">No versions of {name || 'this bundle'} yet. A saved version is immutable. Labels such as prod point at one, and a runtime loads name:label.</p>
      ) : (
        <table className="versions">
          <thead>
            <tr>
              <th>Version</th>
              <th>Saved</th>
              <th>Labels</th>
              <th>Message</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {versions.map((v) => (
              <tr key={v.version} className={bundle?.version === v.version ? 'current' : ''}>
                <td className="mono">{v.version}</td>
                <td>{new Date(v.created_at).toLocaleString()}</td>
                <td>
                  {labelsOf(v.version).map((l) => (
                    <span key={l} className="chip">
                      :{l}
                    </span>
                  ))}
                </td>
                <td>{v.message ?? ''}</td>
                <td className="actions">
                  <button type="button" className="link" onClick={() => open(v.version)}>
                    open
                  </button>
                  <button type="button" className="link" onClick={() => promote('staging', v.version)}>
                    → staging
                  </button>
                  <button type="button" className="link" onClick={() => promote('prod', v.version)}>
                    → prod
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
