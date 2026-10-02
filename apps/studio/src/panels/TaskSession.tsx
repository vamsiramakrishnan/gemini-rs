import { useState } from 'react';
import { isRecord } from '../api';
import type { Json } from '../spec/paths';
import { parseTaskInput, type OperationId, type OperationSnapshot, type SkillSummary, type TaskCommand, type TaskSessionSnapshot, type TaskSnapshot } from '../tasks';
import { FlowState } from './FlowState';

interface Props {
  status: TaskSessionSnapshot;
  disabled: boolean;
  onCommand: (command: TaskCommand) => void;
  onFinish?: (operation: OperationId) => void;
}

export function TaskSession({ status, disabled, onCommand, onFinish }: Props) {
  const [selection, setSelection] = useState('');
  const [returnToParent, setReturnToParent] = useState(true);
  const selected = status.skills.find((skill) => skill.key.name === selection) ?? status.skills[0];
  return (
    <div className="task-session">
      <section className="task-catalog" aria-label="Skill catalog">
        <h3>Start a skill</h3>
        {!selected ? <p className="hint">Add a skill in the Skills section to create tasks.</p> : <>
          <label>Skill to start
            <select aria-label="Skill to start" value={selected.key.name} disabled={disabled} onChange={(event) => setSelection(event.target.value)}>
              {status.skills.map((skill) => <option key={skill.key.name} value={skill.key.name}>{skill.key.name} · {skill.key.version}</option>)}
            </select>
          </label>
          <p className="hint">{selected.description}</p>
          {status.foreground && <label className="row"><input type="checkbox" checked={returnToParent} disabled={disabled} onChange={(event) => setReturnToParent(event.target.checked)} />Return to the current task afterward</label>}
          <JsonCommand key={`${selected.key.name}@${selected.key.version}`} label="Task input" submit="Start task" disabled={disabled} onSubmit={(input) => onCommand({ action: 'start', skill: selected.key.name, input, parent: returnToParent ? status.foreground : null })} />
          <details><summary>Input contract</summary><pre>{JSON.stringify(selected.input_schema, null, 2)}</pre></details>
        </>}
      </section>
      <section className="task-timeline" aria-label="Task timeline">
        <h3>Tasks <span className="count">{status.tasks.length}</span></h3>
        {!status.tasks.length && <p className="hint">Start a task to follow its progress, approvals, and results.</p>}
        {status.tasks.map((task) => <TaskEntry
          key={task.id}
          task={task}
          skill={status.skills.find((skill) => skill.key.name === task.skill.name && skill.key.version === task.skill.version)}
          foreground={status.foreground === task.id}
          operations={status.operations.filter((operation) => operation.owner.task === task.id)}
          disabled={disabled}
          onCommand={onCommand}
          onFinish={onFinish}
        />)}
      </section>
    </div>
  );
}

function TaskEntry({ task, skill, foreground, operations, disabled, onCommand, onFinish }: {
  task: TaskSnapshot;
  skill: SkillSummary | undefined;
  foreground: boolean;
  operations: OperationSnapshot[];
  disabled: boolean;
  onCommand: Props['onCommand'];
  onFinish: Props['onFinish'];
}) {
  const terminal = task.status === 'completed' || task.status === 'cancelled' || task.status === 'failed';
  return (
    <article className={`task-entry ${foreground ? 'foreground' : ''}`} aria-label={`Task ${task.id}: ${task.skill.name}`}>
      <div className="row task-heading">
        <strong>{task.skill.name}</strong>
        <span className="badge">{foreground ? 'Speaking' : task.status}</span>
        <span className="hint">{task.id} · v{task.skill.version} · input {task.revision}{task.parent ? ` · returns to ${task.parent}` : ''}</span>
      </div>
      {task.services_pending && <p className="hint" role="status">Updating task context…</p>}
      {task.service_errors.map((error, index) => <p className="error" role="status" key={index}>{error}</p>)}
      {!terminal && <div className="row">
        {foreground && <button type="button" disabled={disabled} onClick={() => onCommand({ action: 'suspend', task: task.id })}>Suspend</button>}
        {task.status === 'suspended' && <button type="button" disabled={disabled} onClick={() => onCommand({ action: 'resume', task: task.id })}>Resume</button>}
        <button type="button" className="danger" disabled={disabled} onClick={() => onCommand({ action: 'cancel', task: task.id })}>Cancel task</button>
      </div>}
      {operations.map((operation) => <OperationEntry key={operation.id} operation={operation} disabled={disabled || task.services_pending} onCommand={onCommand} onFinish={onFinish} />)}
      {!terminal && <div className="task-actions">
        {foreground && skill && skill.tools.length > 0 && <details><summary>Run a tool</summary><InvokeTool key={`${task.id}:${task.revision}`} task={task} skill={skill} disabled={disabled || task.pending !== null || task.services_pending} onCommand={onCommand} /></details>}
        <details><summary>Revise inputs</summary>
          <p className="hint">Replace this task's inputs and restart its dialogue. Earlier work and pending approvals become stale.</p>
          <JsonCommand key={task.revision} label={`Revised input for ${task.id}`} submit="Apply input revision" disabled={disabled} onSubmit={(input) => onCommand({ action: 'revise', task: task.id, expected_revision: task.revision, input })} />
        </details>
        <details><summary>Complete task</summary>
          <JsonCommand label={`Output for ${task.id}`} submit="Complete task" disabled={disabled || task.pending !== null || task.services_pending} onSubmit={(output) => onCommand({ action: 'complete', task: task.id, output })} />
        </details>
      </div>}
      {task.flow && <details><summary>Dialogue progress</summary><FlowState status={task.flow} /></details>}
      {task.output !== null && <details open><summary>Task output</summary><pre>{JSON.stringify(task.output, null, 2)}</pre></details>}
    </article>
  );
}

function OperationEntry({ operation, disabled, onCommand, onFinish }: {
  operation: OperationSnapshot;
  disabled: boolean;
  onCommand: Props['onCommand'];
  onFinish: Props['onFinish'];
}) {
  return (
    <div className={`task-operation ${operation.status === 'unknown' ? 'uncertain' : ''}`}>
      <div className="row"><strong>{operation.tool}</strong><span className="badge">{operation.status.replaceAll('_', ' ')}</span><span className="hint">{operation.id} · input {operation.owner.revision}</span></div>
      {operation.idempotency_key && <div className="hint">Operation key: <code>{operation.idempotency_key}</code></div>}
      {operation.cancellation_requested && <p className="hint">Cancellation requested. An external effect may already have occurred.</p>}
      {operation.status === 'unknown' && <p className="error" role="status">Outcome unknown. Verify the external result before retrying.</p>}
      {operation.error && <p className="error">{operation.error}</p>}
      {operation.status === 'awaiting_approval' ? <>
        <pre>{JSON.stringify(operation.args, null, 2)}</pre>
        <div className="row">
          <button type="button" className="primary" disabled={disabled} onClick={() => onCommand({ action: 'decide', operation: operation.id, approve: true })}>Approve</button>
          <button type="button" disabled={disabled} onClick={() => onCommand({ action: 'decide', operation: operation.id, approve: false })}>Deny</button>
        </div>
      </> : <details><summary>Arguments{operation.result !== null ? ' and result' : ''}</summary><pre>{JSON.stringify({ args: operation.args, result: operation.result }, null, 2)}</pre></details>}
      {onFinish && operation.status === 'running' && <button type="button" disabled={disabled} onClick={() => onFinish(operation.id)}>Finish mock operation</button>}
    </div>
  );
}

function InvokeTool({ task, skill, disabled, onCommand }: { task: TaskSnapshot; skill: SkillSummary; disabled: boolean; onCommand: Props['onCommand'] }) {
  const [selection, setSelection] = useState('');
  const blocked = task.flow?.blocked_tools ?? {};
  const available = skill.tools.filter((tool) => !(tool.name in blocked));
  const selected = available.find((tool) => tool.name === selection) ?? available[0];
  if (!selected) return <p className="hint">No tools are available at this step. Check Dialogue progress for the remaining requirements.</p>;
  return <>
    <label>Tool
      <select aria-label="Tool" value={selected.name} disabled={disabled} onChange={(event) => setSelection(event.target.value)}>
        {skill.tools.map((tool) => <option key={tool.name} value={tool.name} disabled={tool.name in blocked} title={blocked[tool.name]}>{tool.name}{tool.name in blocked ? ' (unavailable)' : ''}</option>)}
      </select>
    </label>
    <p className="hint">{selected.description}</p>
    {selected.effect.kind === 'commit' && <p className="hint">Requires approval. Include a stable <code>{selected.effect.idempotency_argument}</code> string in the arguments.</p>}
    <JsonCommand key={selected.name} label={`Arguments for ${selected.name}`} submit="Invoke tool" disabled={disabled} onSubmit={(args) => {
      const key = selected.effect.kind === 'commit' && isRecord(args) ? args[selected.effect.idempotency_argument] : null;
      onCommand({ action: 'invoke', task: task.id, tool: selected.name, args, idempotency_key: typeof key === 'string' ? key : null });
    }} />
    <details><summary>Argument contract</summary><pre>{JSON.stringify(selected.parameters, null, 2)}</pre></details>
  </>;
}

function JsonCommand({ label, submit, disabled, onSubmit }: { label: string; submit: string; disabled: boolean; onSubmit: (value: Json) => void }) {
  const [draft, setDraft] = useState('{}');
  const [error, setError] = useState<string | null>(null);
  return <form className="task-input" onSubmit={(event) => {
    event.preventDefault();
    if (disabled) return;
    try {
      const input = parseTaskInput(draft);
      setError(null);
      onSubmit(input);
    } catch (error) {
      setError(error instanceof Error ? error.message : 'Enter valid JSON.');
    }
  }}>
    <label>{label}<textarea aria-label={label} rows={3} value={draft} disabled={disabled} onChange={(event) => setDraft(event.target.value)} spellCheck={false} /></label>
    {error && <p className="error" role="alert">{error}</p>}
    <button type="submit" disabled={disabled}>{submit}</button>
  </form>;
}
