import type { FlowStatus, Trace } from '../api';

/** Active steps, admitted and blocked tools, and the guard truth tree of
 * each active step: exactly which atom it is waiting on. With `declared`
 * (`contextUpdate` steering) the admitted tools are the only ones the model
 * is offered; otherwise it sees every tool and the rest are refused. */
export function FlowState({ status, declared = false }: { status: FlowStatus; declared?: boolean }) {
  const blocked = Object.entries(status.blocked_tools ?? {});
  const progress = Object.entries(status.active_progress ?? {});
  return (
    <div className="flow-state">
      <dl>
        <dt>Status</dt>
        <dd>{status.terminated ? 'Terminated' : status.complete ? 'Complete' : 'In progress'}</dd>
        {status.overlay_path.length > 0 && <><dt>Digression</dt><dd>{status.overlay_path.join(' → ')}</dd></>}
        <dt>Active</dt>
        <dd>{status.active?.join(', ') || '—'}</dd>
        <dt title={declared ? 'Declared to the model through contextUpdate' : 'Calls to other tools are refused'}>
          {declared ? 'Tools declared' : 'Tools allowed'}
        </dt>
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
