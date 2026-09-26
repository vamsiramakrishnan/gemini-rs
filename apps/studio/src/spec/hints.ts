// Suggestions for fields that name something else in the spec: a tool, a
// step or stage, or a state key. Offered as completions, never enforced;
// validation is the server's job.

import { graphOf, type Spec } from './graph';
import { isObject, type Json } from './paths';

export interface Names {
  tools: string[];
  nodes: string[];
  stateKeys: string[];
}

function collectStrings(value: Json | undefined, into: Set<string>) {
  if (Array.isArray(value)) for (const v of value) if (typeof v === 'string') into.add(v);
}

export function namesIn(spec: Spec): Names {
  const tools = new Set<string>();
  const stateKeys = new Set<string>();
  for (const tool of Array.isArray(spec.tools) ? spec.tools : []) {
    if (!isObject(tool)) continue;
    if (typeof tool.name === 'string') tools.add(tool.name);
    if (isObject(tool.set_state)) for (const key of Object.keys(tool.set_state)) stateKeys.add(key);
    if (typeof tool.save_response_as === 'string') stateKeys.add(tool.save_response_as);
  }
  if (isObject(spec.state)) for (const key of Object.keys(spec.state)) stateKeys.add(key);
  if (isObject(spec.computed)) for (const key of Object.keys(spec.computed)) stateKeys.add(key);
  const walk = (value: Json | undefined) => {
    if (Array.isArray(value)) value.forEach(walk);
    else if (isObject(value)) {
      for (const [key, inner] of Object.entries(value)) {
        if (key === 'collect' || key === 'captured') collectStrings(inner, stateKeys);
        else if ((key === 'is_true' || key === 'is_set' || key === 'state_key') && typeof inner === 'string') stateKeys.add(inner);
        walk(inner);
      }
    }
  };
  walk(spec.conversation);
  walk(spec.flow);
  walk(spec.extract);
  return { tools: [...tools].sort(), nodes: graphOf(spec).nodes.map((n) => n.id), stateKeys: [...stateKeys].sort() };
}

const TOOL_FIELDS = new Set(['allow', 'deny', 'ambient', 'confirm_tools', 'tool', 'called_ok', 'resolver', 'tools']);
const NODE_FIELDS = new Set(['after', 'to', 'done', 'require', 'escalate_to', 'step', 'initial', 'from']);
const STATE_FIELDS = new Set(['is_true', 'is_set', 'captured', 'collect', 'save_response_as', 'state_key', 'key', 'keys', 'slot']);

/** What a field named `key` refers to, if anything. */
export function suggestionsFor(key: string | undefined, names: Names): string[] | undefined {
  if (!key) return undefined;
  if (TOOL_FIELDS.has(key)) return names.tools;
  if (NODE_FIELDS.has(key)) return names.nodes;
  if (STATE_FIELDS.has(key)) return names.stateKeys;
  return undefined;
}

/** Fields whose text is prose and deserves a multi-line box. */
export const LONG_TEXT = new Set([
  'instruction',
  'say',
  'posture',
  'ground',
  'greeting',
  'description',
  'prompt',
  'reprompt',
  'template',
  'text',
]);
