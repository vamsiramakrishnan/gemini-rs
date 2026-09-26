// The canvas view of a spec: steps of a `flow`, or stages (and overlay
// stages) of a `conversation`, with the edges between them. Every canvas
// edit is an edit of the spec document; positions are view state only.

import { isObject, type Json, type JsonObject } from './paths';

export type Spec = JsonObject;
export type Mode = 'flow' | 'conversation';

export interface GraphNode {
  id: string;
  kind: 'step' | 'stage' | 'overlay';
  /** The overlay a stage belongs to, for overlay stages. */
  overlay?: string;
  terminal: boolean;
  /** What the node is: its instruction/posture, one line. */
  say?: string;
  /** Completion, one line. */
  done?: string;
  /** Tools admitted while the node is active. */
  allow: string[];
  /** Slots it collects (stages). */
  collect: string[];
  /** A confirm-before-act tool (stages). */
  commit?: string;
}

export interface GraphEdge {
  id: string;
  source: string;
  target: string;
  kind: 'after' | 'next' | 'escalate';
  label?: string;
}

export function modeOf(spec: Spec): Mode {
  return isObject(spec.conversation) ? 'conversation' : 'flow';
}

function asArray(value: Json | undefined): Json[] {
  return Array.isArray(value) ? value : [];
}

function strings(value: Json | undefined): string[] {
  return asArray(value).filter((v): v is string => typeof v === 'string');
}

/** A guard as one readable line: `captured(party_size, slot)`, `all(...)`. */
export function guardSummary(pred: Json | undefined): string {
  if (pred === undefined || pred === null) return '';
  if (typeof pred === 'string') return pred;
  if (!isObject(pred)) return JSON.stringify(pred);
  const [key, value] = Object.entries(pred)[0] ?? ['', null];
  switch (key) {
    case 'all':
    case 'any':
      return `${key}(${asArray(value).map(guardSummary).join(', ')})`;
    case 'not':
      return `not(${guardSummary(value)})`;
    case 'captured':
      return `captured(${strings(value).join(', ')})`;
    case 'eq': {
      const [k, v] = asArray(value);
      return `${String(k)} = ${JSON.stringify(v)}`;
    }
    default:
      return `${key}(${typeof value === 'string' ? value : JSON.stringify(value)})`;
  }
}

function stepsOf(spec: Spec): JsonObject[] {
  const flow = spec.flow;
  return isObject(flow) ? asArray(flow.steps).filter(isObject) : [];
}

function conversationOf(spec: Spec): JsonObject {
  return isObject(spec.conversation) ? spec.conversation : {};
}

function stagesOf(spec: Spec): JsonObject[] {
  return asArray(conversationOf(spec).stages).filter(isObject);
}

function overlaysOf(spec: Spec): JsonObject[] {
  return asArray(conversationOf(spec).overlays).filter(isObject);
}

function edgeTarget(edge: Json): string | undefined {
  if (typeof edge === 'string') return edge;
  if (isObject(edge) && typeof edge.step === 'string') return edge.step;
  return undefined;
}

export function graphOf(spec: Spec): { nodes: GraphNode[]; edges: GraphEdge[] } {
  const nodes: GraphNode[] = [];
  const edges: GraphEdge[] = [];
  if (modeOf(spec) === 'flow') {
    for (const step of stepsOf(spec)) {
      const id = String(step.id ?? '');
      nodes.push({
        id,
        kind: 'step',
        terminal: step.terminal === true,
        say: typeof step.posture === 'string' ? step.posture : undefined,
        done: guardSummary(step.done) || undefined,
        allow: strings(step.allow),
        collect: [],
      });
      for (const dep of asArray(step.after)) {
        const source = edgeTarget(dep);
        if (!source) continue;
        const when = isObject(dep) ? guardSummary(dep.when) : '';
        edges.push({ id: `after:${source}->${id}`, source, target: id, kind: 'after', label: when || undefined });
      }
    }
    return { nodes, edges };
  }
  const addStage = (stage: JsonObject, overlay?: string) => {
    const id = String(stage.id ?? '');
    const commit = isObject(stage.commit) && typeof stage.commit.tool === 'string' ? stage.commit.tool : undefined;
    nodes.push({
      id,
      kind: overlay ? 'overlay' : 'stage',
      overlay,
      terminal: stage.terminal === true,
      say: typeof stage.say === 'string' ? stage.say : typeof stage.instruction === 'string' ? stage.instruction : undefined,
      done: guardSummary(stage.done) || undefined,
      allow: strings(stage.allow),
      collect: strings(stage.collect),
      commit,
    });
    asArray(stage.next).forEach((transition, i) => {
      if (!isObject(transition) || typeof transition.to !== 'string') return;
      edges.push({
        id: `next:${id}->${transition.to}:${i}`,
        source: id,
        target: transition.to,
        kind: 'next',
        label: guardSummary(transition.when) || undefined,
      });
    });
    for (const dep of strings(stage.after)) {
      edges.push({ id: `after:${dep}->${id}`, source: dep, target: id, kind: 'after' });
    }
    const repair = stage.repair;
    if (isObject(repair) && typeof repair.escalate_to === 'string') {
      edges.push({ id: `escalate:${id}->${repair.escalate_to}`, source: id, target: repair.escalate_to, kind: 'escalate', label: 'escalate' });
    }
  };
  for (const stage of stagesOf(spec)) addStage(stage);
  for (const overlay of overlaysOf(spec)) {
    for (const stage of asArray(overlay.stages).filter(isObject)) addStage(stage, String(overlay.name ?? ''));
  }
  return { nodes, edges };
}

// ── Edits ───────────────────────────────────────────────────────────────────

function withSteps(spec: Spec, steps: JsonObject[]): Spec {
  const flow = isObject(spec.flow) ? spec.flow : {};
  return { ...spec, flow: { ...flow, steps } };
}

function withStages(spec: Spec, stages: JsonObject[]): Spec {
  return { ...spec, conversation: { ...conversationOf(spec), stages } };
}

/** Every node id in the spec, overlay stages included. */
export function nodeIds(spec: Spec): string[] {
  return graphOf(spec).nodes.map((n) => n.id);
}

export function uniqueId(spec: Spec, base: string): string {
  const taken = new Set(nodeIds(spec));
  if (!taken.has(base)) return base;
  for (let n = 2; ; n++) if (!taken.has(`${base}_${n}`)) return `${base}_${n}`;
}

export function addNode(spec: Spec, base?: string): { spec: Spec; id: string } {
  const mode = modeOf(spec);
  const id = uniqueId(spec, base ?? (mode === 'flow' ? 'step' : 'stage'));
  if (mode === 'flow') return { spec: withSteps(spec, [...stepsOf(spec), { id }]), id };
  return { spec: withStages(spec, [...stagesOf(spec), { id }]), id };
}

/** Make `target` follow `source`: a guarded `next` transition in a
 * conversation (guard `always` until edited), an `after` edge in a flow. */
export function connect(spec: Spec, source: string, target: string): Spec {
  if (source === target) return spec;
  if (modeOf(spec) === 'flow') {
    return withSteps(
      spec,
      stepsOf(spec).map((step) => {
        if (step.id !== target) return step;
        const after = asArray(step.after);
        if (after.some((e) => edgeTarget(e) === source)) return step;
        return { ...step, after: [...after, source] };
      }),
    );
  }
  return withStages(
    spec,
    stagesOf(spec).map((stage) => {
      if (stage.id !== source) return stage;
      const next = asArray(stage.next);
      if (next.some((t) => isObject(t) && t.to === target)) return stage;
      return { ...stage, next: [...next, { to: target, when: 'always' }] };
    }),
  );
}

export function disconnect(spec: Spec, edge: GraphEdge): Spec {
  const drop = (list: Json | undefined, test: (item: Json) => boolean) => {
    const kept = asArray(list).filter((item) => !test(item));
    return kept.length ? kept : undefined;
  };
  const clean = (object: JsonObject, key: string, value: Json[] | undefined): JsonObject => {
    const out = { ...object };
    if (value === undefined) delete out[key];
    else out[key] = value;
    return out;
  };
  if (modeOf(spec) === 'flow') {
    return withSteps(
      spec,
      stepsOf(spec).map((step) =>
        step.id === edge.target ? clean(step, 'after', drop(step.after, (e) => edgeTarget(e) === edge.source)) : step,
      ),
    );
  }
  return withStages(
    spec,
    stagesOf(spec).map((stage) => {
      if (edge.kind === 'next' && stage.id === edge.source) {
        return clean(stage, 'next', drop(stage.next, (t) => isObject(t) && t.to === edge.target));
      }
      if (edge.kind === 'after' && stage.id === edge.target) {
        return clean(stage, 'after', drop(stage.after, (d) => d === edge.source));
      }
      if (edge.kind === 'escalate' && stage.id === edge.source && isObject(stage.repair)) {
        const { escalate_to: _dropped, ...repair } = stage.repair;
        return { ...stage, repair };
      }
      return stage;
    }),
  );
}

/** Rename a node and every reference to it. */
export function renameNode(spec: Spec, from: string, to: string): Spec {
  if (!to || from === to || nodeIds(spec).includes(to)) return spec;
  const swap = (v: Json): Json => (v === from ? to : v);
  const swapGuard = (pred: Json | undefined): Json | undefined => {
    if (!isObject(pred)) return pred;
    const [key, value] = Object.entries(pred)[0] ?? [];
    if (key === 'done' && value === from) return { done: to };
    if ((key === 'all' || key === 'any') && Array.isArray(value)) return { [key]: value.map((p) => swapGuard(p) ?? null) };
    if (key === 'not') return { not: swapGuard(value) ?? null };
    return pred;
  };
  if (modeOf(spec) === 'flow') {
    const steps = stepsOf(spec).map((step) => {
      const out: JsonObject = { ...step, id: swap(step.id ?? null) };
      if (step.after) {
        out.after = asArray(step.after).map((e) =>
          isObject(e)
            ? { ...e, step: swap(e.step ?? null), ...(e.when !== undefined ? { when: swapGuard(e.when) ?? null } : {}) }
            : swap(e),
        );
      }
      for (const key of ['done', 'gate']) if (step[key] !== undefined) out[key] = swapGuard(step[key]) ?? null;
      return out;
    });
    return withSteps(spec, steps);
  }
  const renameStage = (stage: JsonObject): JsonObject => {
    const out: JsonObject = { ...stage, id: swap(stage.id ?? null) };
    if (stage.after) out.after = asArray(stage.after).map(swap);
    if (stage.next) {
      out.next = asArray(stage.next).map((t) =>
        isObject(t) ? { ...t, to: swap(t.to ?? null), ...(t.when !== undefined ? { when: swapGuard(t.when) ?? null } : {}) } : t,
      );
    }
    if (stage.done !== undefined) out.done = swapGuard(stage.done) ?? null;
    if (isObject(stage.repair) && stage.repair.escalate_to === from) out.repair = { ...stage.repair, escalate_to: to };
    return out;
  };
  const conversation = conversationOf(spec);
  const out: JsonObject = { ...conversation, stages: stagesOf(spec).map(renameStage) };
  if (conversation.require) out.require = asArray(conversation.require).map(swap);
  if (conversation.overlays) {
    out.overlays = overlaysOf(spec).map((o) => ({ ...o, stages: asArray(o.stages).filter(isObject).map(renameStage) }));
  }
  return { ...spec, conversation: out };
}

/** Remove a node and every reference to it. */
export function removeNode(spec: Spec, id: string): Spec {
  const { edges } = graphOf(spec);
  let out = spec;
  for (const edge of edges) if (edge.source === id || edge.target === id) out = disconnect(out, edge);
  if (modeOf(out) === 'flow') return withSteps(out, stepsOf(out).filter((s) => s.id !== id));
  const conversation = conversationOf(out);
  const next: JsonObject = { ...conversation, stages: stagesOf(out).filter((s) => s.id !== id) };
  if (conversation.require) next.require = strings(conversation.require).filter((r) => r !== id);
  if (conversation.overlays) {
    next.overlays = overlaysOf(out).map((o) => ({ ...o, stages: asArray(o.stages).filter((s) => !isObject(s) || s.id !== id) }));
  }
  return { ...out, conversation: next };
}

/** The path of a node's object in the spec, for the inspector. */
export function nodePath(spec: Spec, id: string): (string | number)[] | undefined {
  if (modeOf(spec) === 'flow') {
    const index = stepsOf(spec).findIndex((s) => s.id === id);
    return index < 0 ? undefined : ['flow', 'steps', index];
  }
  const index = stagesOf(spec).findIndex((s) => s.id === id);
  if (index >= 0) return ['conversation', 'stages', index];
  const overlays = overlaysOf(spec);
  for (let o = 0; o < overlays.length; o++) {
    const stages = asArray(overlays[o]!.stages);
    const i = stages.findIndex((s) => isObject(s) && s.id === id);
    if (i >= 0) return ['conversation', 'overlays', o, 'stages', i];
  }
  return undefined;
}

/** Convert a flow spec to a conversation (or start one), keeping tools and
 * everything else. Steps become stages; `after` edges become transitions
 * guarded by the source's completion. */
export function toConversation(spec: Spec): Spec {
  if (modeOf(spec) === 'conversation') return spec;
  const steps = stepsOf(spec);
  const stages: JsonObject[] = steps.map((step) => {
    const stage: JsonObject = { id: step.id ?? null };
    if (typeof step.posture === 'string') stage.say = step.posture;
    if (typeof step.ground === 'string') stage.ground = step.ground;
    if (step.allow) stage.allow = step.allow;
    if (step.done !== undefined) stage.done = step.done;
    if (step.terminal === true) stage.terminal = true;
    // A dependency becomes a transition out of its source, guarded by the
    // source's completion and, for a conditional edge, by its condition.
    const next = steps.flatMap((s) =>
      asArray(s.after)
        .filter((e) => edgeTarget(e) === step.id)
        .map((e) => {
          const done: Json = { done: step.id ?? null };
          const when = isObject(e) && e.when !== undefined && e.when !== null ? { all: [done, e.when] } : done;
          return { to: s.id ?? null, when };
        }),
    );
    if (next.length) stage.next = next;
    return stage;
  });
  const { flow: _flow, ...rest } = spec;
  return {
    ...rest,
    conversation: { name: typeof spec.name === 'string' && spec.name ? spec.name : 'conversation', stages: stages.length ? stages : [{ id: 'start' }] },
  };
}
