import { useStudio } from '../store';
import { graphOf, modeOf } from '../spec/graph';
import { isObject } from '../spec/paths';
import { visibleSections } from './Inspector';

function count(value: unknown): number | null {
  if (Array.isArray(value)) return value.length;
  if (isObject(value)) return Object.keys(value).length;
  return null;
}

export function Outline() {
  const spec = useStudio((s) => s.spec);
  const selection = useStudio((s) => s.selection);
  const select = useStudio((s) => s.select);
  const graph = graphOf(spec);
  const current = selection?.kind === 'section' ? selection.section : selection ? null : 'agent';
  return (
    <nav className="outline">
      <div className="outline-group">
        <div className="outline-head">Sections</div>
        {visibleSections(spec).map((section) => {
          const n = section.property ? count(spec[section.property]) : null;
          return (
            <button
              key={section.key}
              type="button"
              className={current === section.key ? 'active' : ''}
              onClick={() => select({ kind: 'section', section: section.key })}
            >
              {section.label}
              {n !== null && section.key !== 'conversation' && section.key !== 'flow' && <span className="count">{n}</span>}
            </button>
          );
        })}
      </div>
      <div className="outline-group">
        <div className="outline-head">{modeOf(spec) === 'flow' ? 'Steps' : 'Stages'}</div>
        {graph.nodes.map((node) => (
          <button
            key={`${node.overlay ?? ''}:${node.id}`}
            type="button"
            className={selection?.kind === 'node' && selection.id === node.id ? 'active' : ''}
            onClick={() => select({ kind: 'node', id: node.id })}
          >
            {node.overlay ? `${node.overlay} › ` : ''}
            {node.id || '(no id)'}
          </button>
        ))}
      </div>
    </nav>
  );
}
