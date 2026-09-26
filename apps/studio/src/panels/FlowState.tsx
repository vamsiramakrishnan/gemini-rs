import type { FlowStatus, Trace } from '../api';

/** Active steps, admitted and blocked tools, and the guard truth tree of
 * each active step: exactly which atom it is waiting on. */
export function FlowState({ status }: { status: FlowStatus }) {
  const blocked = Object.entries(status.blocked_tools ?? {});
  const progress = Object.entries(status.active_progress ?? {});
  return (
    <div className="flow-state">
      <dl>
        <dt>Active</dt>
        <dd>{status.active?.join(', ') || '—'}</dd>
        <dt>Tools allowed</dt>
        <dd>{status.allowed_tools?.join(', ') || '—'}</dd>
        <dt>Missing</dt>
        <dd>{status.missing_requirements?.join(', ') || 'none'}</dd>
        {blocked.length > 0 && (
          <>
            <dt>Blocked</dt>
            <dd>
              {blocked.map(([tool, reason]) => (
                <div key={tool}>
                  <b>{tool}</b> — {reason}
                </div>
              ))}
            </dd>
          </>
        )}
      </dl>
      {progress.map(([step, trace]) => (
        <div key={step} className="trace">
          <div className="trace-step">{step}</div>
          <TraceNode node={trace} depth={0} />
        </div>
      ))}
    </div>
  );
}

function TraceNode({ node, depth }: { node: Trace; depth: number }) {
  return (
    <>
      <div className={`trace-node ${node.holds ? 'holds' : 'waiting'}`} style={{ marginLeft: depth * 14 }}>
        {node.holds ? '●' : '○'} {node.desc}
      </div>
      {(node.children ?? []).map((child, i) => (
        <TraceNode key={i} node={child} depth={depth + 1} />
      ))}
    </>
  );
}
