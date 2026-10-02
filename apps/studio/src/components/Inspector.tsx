// Edit whatever is selected: a node, a transition, or a section of the spec.

import { useEffect, useMemo, useState } from 'react';
import { pathKey, useStudio } from '../store';
import { disconnect, graphOf, isSkillsOnly, modeOf, nodePath, removeNode, renameNode, toConversation, type Spec } from '../spec/graph';
import { getIn, isObject, setIn, type Json, type JsonObject, type Path } from '../spec/paths';
import { describe, resolve, unwrapNullable, type RootSchema, type Schema } from '../spec/schema';
import { namesIn } from '../spec/hints';
import { Field, ObjectForm } from './SchemaForm';
import { sendPostures } from '../live';

export interface Section {
  key: string;
  label: string;
  /** Top-level properties shown together (the "agent" section). */
  fields?: string[];
  /** A single top-level property. */
  property?: string;
  /** Properties of that property to leave out (edited elsewhere). */
  omit?: string[];
}

export const SECTIONS: Section[] = [
  { key: 'agent', label: 'Agent', fields: ['name', 'description', 'version', 'instruction', 'greeting', 'modality', 'voice', 'initial_phase'] },
  { key: 'skills', label: 'Skills', property: 'skills' },
  { key: 'conversation', label: 'Conversation', property: 'conversation', omit: ['stages'] },
  { key: 'flow', label: 'Flow', property: 'flow', omit: ['steps'] },
  { key: 'tools', label: 'Tools', property: 'tools' },
  { key: 'state', label: 'State', property: 'state' },
  { key: 'computed', label: 'Computed', property: 'computed' },
  { key: 'extract', label: 'Extraction', property: 'extract' },
  { key: 'memory', label: 'Memory', property: 'memory' },
  { key: 'phases', label: 'Phases', property: 'phases' },
  { key: 'watch', label: 'Watchers', property: 'watch' },
  { key: 'patterns', label: 'Patterns', property: 'patterns' },
  { key: 'runtime', label: 'Runtime', property: 'runtime' },
  { key: 'mcp', label: 'MCP servers', property: 'mcp' },
  { key: 'tests', label: 'Flow tests', property: 'tests' },
  { key: 'scenarios', label: 'Scenarios', property: 'scenarios' },
  { key: 'task_scenarios', label: 'Task scenarios', property: 'task_scenarios' },
  { key: 'fragments', label: 'Fragments', property: 'fragments' },
];

export function visibleSections(spec: Spec): Section[] {
  if (isSkillsOnly(spec)) {
    return SECTIONS.filter((section) => {
      if (['agent', 'skills', 'runtime', 'task_scenarios'].includes(section.key)) return true;
      // Keep existing fields reachable for repair, but author new services inside skills.
      const value = section.property ? spec[section.property] : undefined;
      return Array.isArray(value) ? value.length > 0 : isObject(value) && Object.keys(value).length > 0;
    });
  }
  const mode = modeOf(spec);
  return SECTIONS.filter((s) => (mode === 'flow' ? s.key !== 'conversation' : s.key !== 'flow'));
}

function without(schema: Schema, omit: string[] = []): Schema {
  if (!omit.length || !schema.properties) return schema;
  const properties = { ...schema.properties };
  for (const key of omit) delete properties[key];
  return { ...schema, properties, required: (schema.required ?? []).filter((r) => !omit.includes(r)) };
}

export function Inspector() {
  const spec = useStudio((s) => s.spec);
  const schema = useStudio((s) => s.schema);
  const selection = useStudio((s) => s.selection);
  const names = useMemo(() => namesIn(spec), [spec]);
  if (!schema) return <aside className="inspector">Loading schema…</aside>;
  let body;
  if (selection?.kind === 'node') body = <NodeInspector id={selection.id} root={schema} names={names} />;
  else if (selection?.kind === 'edge') body = <EdgeInspector id={selection.id} root={schema} names={names} />;
  else if (selection?.kind === 'skill') body = <SkillInspector index={selection.index} root={schema} />;
  else {
    const key = selection?.kind === 'section' ? selection.section : 'agent';
    const section = SECTIONS.find((s) => s.key === key) ?? SECTIONS[0]!;
    body = <SectionInspector section={section} root={schema} names={names} />;
  }
  return <aside className="inspector">{body}</aside>;
}

type Names = ReturnType<typeof namesIn>;

function SkillInspector({ index, root }: { index: number; root: RootSchema }) {
  const spec = useStudio((state) => state.spec);
  const editAt = useEditAt();
  const skill = Array.isArray(spec.skills) ? spec.skills[index] : undefined;
  if (!isObject(skill)) return <p className="hint">This skill no longer exists.</p>;
  const skillSchema = resolve(root, { $ref: '#/definitions/SkillSpec' });
  const primaryFields = ['name', 'version', 'description', 'instruction', 'inputs', 'outputs', 'state', 'tools', 'conversation', 'flow', 'tests'];
  const fields = [...primaryFields, ...Object.keys(skillSchema.properties ?? {}).filter((key) => !primaryFields.includes(key))];
  return <div className="inspector-body">
    <div className="inspector-title"><h2>{typeof skill.name === 'string' ? skill.name : 'Skill'}</h2><button type="button" className="danger" onClick={() => {
      editAt(['skills', index], undefined);
      useStudio.getState().select({ kind: 'section', section: 'skills' });
    }}>Delete skill</button></div>
    <p className="hint">Reusable definition. Each task gets its own inputs, progress, and tool bindings.</p>
    {skillSchema.properties ? <ObjectForm root={root} schema={skillSchema} only={fields} value={skill} names={namesIn(skill)} required onChange={(value) => editAt(['skills', index], value)} /> : <p className="error">The server schema does not include skills. Restart the web server with the current SDK.</p>}
  </div>;
}

function useEditAt() {
  const spec = useStudio((s) => s.spec);
  const edit = useStudio((s) => s.edit);
  return (path: Path, value: Json | undefined) => edit(setIn(spec, path, value), pathKey(path));
}

function SectionInspector({ section, root, names }: { section: Section; root: RootSchema; names: Names }) {
  const spec = useStudio((s) => s.spec);
  const edit = useStudio((s) => s.edit);
  const editAt = useEditAt();
  const top = resolve(root, root);
  if (section.fields) {
    return (
      <div className="inspector-body">
        <h2>{section.label}</h2>
        <ObjectForm
          root={root}
          schema={top}
          only={section.fields}
          value={spec}
          onChange={(next) => edit((next ?? {}) as Spec, 'agent')}
          names={names}
          required
        />
      </div>
    );
  }
  const property = section.property!;
  const propertySchema = top.properties?.[property];
  const resolved = resolve(root, propertySchema);
  return (
    <div className="inspector-body">
      <h2>{section.label}</h2>
      {section.key === 'flow' && (
        <p className="hint">
          This agent uses a step flow.{' '}
          <button type="button" className="link" onClick={() => edit(toConversation(spec))}>
            Convert to a conversation
          </button>{' '}
          for slots, commits, repair and overlays.
        </p>
      )}
      {describe(resolved.description) && <p className="hint">{describe(resolved.description)}</p>}
      {section.omit ? (
        isObject(spec[property]) ? (
          <ObjectForm
            root={root}
            schema={without(unwrapNullable(root, propertySchema).schema, section.omit)}
            value={omitKeys(spec[property] as JsonObject, section.omit)}
            onChange={(next) =>
              editAt([property], { ...pick(spec[property] as JsonObject, section.omit!), ...(isObject(next) ? next : {}) })
            }
            names={names}
            required
          />
        ) : null
      ) : (
        <Field root={root} schema={propertySchema} value={spec[property]} onChange={(next) => editAt([property], next)} names={names} name={property} bare />
      )}
    </div>
  );
}

function omitKeys(object: JsonObject, keys: string[]): JsonObject {
  const out = { ...object };
  for (const key of keys) delete out[key];
  return out;
}

function pick(object: JsonObject, keys: string[]): JsonObject {
  const out: JsonObject = {};
  for (const key of keys) if (object[key] !== undefined) out[key] = object[key]!;
  return out;
}

function NodeInspector({ id, root, names }: { id: string; root: RootSchema; names: Names }) {
  const spec = useStudio((s) => s.spec);
  const edit = useStudio((s) => s.edit);
  const select = useStudio((s) => s.select);
  const path = nodePath(spec, id);
  const [draft, setDraft] = useState(id);
  useEffect(() => setDraft(id), [id]);
  if (!path) return <p className="hint">This node no longer exists.</p>;
  const mode = modeOf(spec);
  const definition = mode === 'flow' ? 'Step' : 'StageSpec';
  const nodeSchema = without(resolve(root, { $ref: `#/definitions/${definition}` }), ['id']);
  const value = getIn(spec, path);
  const commitRename = () => {
    const to = draft.trim();
    if (!to || to === id) return setDraft(id);
    const next = renameNode(spec, id, to);
    if (next === spec) return setDraft(id);
    edit(next);
    select({ kind: 'node', id: to });
  };
  return (
    <div className="inspector-body">
      <div className="inspector-title">
        <span className="kind">{mode === 'flow' ? 'Step' : 'Stage'}</span>
        <input
          className="id-input"
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onBlur={commitRename}
          onKeyDown={(e) => e.key === 'Enter' && commitRename()}
          aria-label="id"
        />
        <button
          type="button"
          className="danger"
          onClick={() => {
            edit(removeNode(spec, id));
            select(null);
          }}
        >
          Delete
        </button>
      </div>
      <ObjectForm
        root={root}
        schema={nodeSchema}
        value={value}
        onChange={(next) => {
          const full = { ...(isObject(next) ? next : {}), id } as JsonObject;
          const previousRevision = useStudio.getState().revision;
          edit(setIn(spec, path, full), pathKey(path));
          if (mode === 'flow' && isObject(value)) {
            const keys = new Set([...Object.keys(value), ...Object.keys(full)]);
            const onlyPrompts = [...keys].every((key) => key === 'posture' || key === 'ground' || JSON.stringify(value[key]) === JSON.stringify(full[key]));
            if (!onlyPrompts) return;
            const postures: Record<string, string> = {};
            const grounds: Record<string, string> = {};
            if (full.posture !== value.posture) postures[id] = typeof full.posture === 'string' ? full.posture : '';
            if (full.ground !== value.ground) grounds[id] = typeof full.ground === 'string' ? full.ground : '';
            if (Object.keys(postures).length || Object.keys(grounds).length) {
              sendPostures({ postures, grounds, previousRevision, revision: useStudio.getState().revision });
            }
          }
        }}
        names={names}
        required
      />
    </div>
  );
}

function EdgeInspector({ id, root, names }: { id: string; root: RootSchema; names: Names }) {
  const spec = useStudio((s) => s.spec);
  const edit = useStudio((s) => s.edit);
  const editAt = useEditAt();
  const edge = graphOf(spec).edges.find((e) => e.id === id);
  if (!edge) return <p className="hint">This transition no longer exists.</p>;
  const sourcePath = nodePath(spec, edge.kind === 'after' ? edge.target : edge.source);
  let body = null;
  if (edge.kind === 'next' && sourcePath) {
    const next = getIn(spec, [...sourcePath, 'next']);
    const index = Array.isArray(next) ? next.findIndex((t) => isObject(t) && t.to === edge.target) : -1;
    if (index >= 0) {
      const path = [...sourcePath, 'next', index];
      body = (
        <Field
          root={root}
          schema={{ $ref: '#/definitions/TransitionSpec2' }}
          value={getIn(spec, path)}
          onChange={(v) => editAt(path, v)}
          names={names}
          required
          bare
        />
      );
    }
  } else if (edge.kind === 'after' && sourcePath && modeOf(spec) === 'flow') {
    const after = getIn(spec, [...sourcePath, 'after']);
    const index = Array.isArray(after) ? after.findIndex((e) => e === edge.source || (isObject(e) && e.step === edge.source)) : -1;
    if (index >= 0) {
      const path = [...sourcePath, 'after', index];
      body = (
        <Field root={root} schema={{ $ref: '#/definitions/Edge' }} value={getIn(spec, path)} onChange={(v) => editAt(path, v)} names={names} required bare />
      );
    }
  } else if (edge.kind === 'escalate') {
    body = <p className="hint">Repair escalates here after too many failed attempts. Edit it in the stage's repair settings.</p>;
  }
  return (
    <div className="inspector-body">
      <div className="inspector-title">
        <span className="kind">{edge.kind === 'next' ? 'Transition' : edge.kind === 'after' ? 'Dependency' : 'Escalation'}</span>
        <span className="edge-ends">
          {edge.source} → {edge.target}
        </span>
        <button
          type="button"
          className="danger"
          onClick={() => {
            edit(disconnect(spec, edge));
            useStudio.getState().select(null);
          }}
        >
          Delete
        </button>
      </div>
      {body}
    </div>
  );
}
