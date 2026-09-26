import { describe, expect, it } from 'vitest';
import schemaJson from './session-spec.schema.json';
import { getIn, prune, setIn } from '../spec/paths';
import { defaultFor, kindOf, resolve, unwrapNullable, variantOf, variants, type RootSchema } from '../spec/schema';
import {
  addNode,
  connect,
  disconnect,
  graphOf,
  guardSummary,
  modeOf,
  nodePath,
  removeNode,
  renameNode,
  toConversation,
  type Spec,
} from '../spec/graph';

const root = schemaJson as unknown as RootSchema;
const prop = (name: string) => root.properties![name]!;

describe('paths', () => {
  it('sets and removes without touching the original', () => {
    const doc = { a: { b: [1, 2, 3] } };
    const next = setIn(doc, ['a', 'b', 1], 9);
    expect(next).toEqual({ a: { b: [1, 9, 3] } });
    expect(doc.a.b[1]).toBe(2);
    expect(setIn(doc, ['a', 'b', 0], undefined)).toEqual({ a: { b: [2, 3] } });
    expect(setIn({}, ['x', 'y'], 'z')).toEqual({ x: { y: 'z' } });
    expect(getIn(next, ['a', 'b', 1])).toBe(9);
  });

  it('prunes what was not authored', () => {
    expect(prune({ a: '', b: [], c: {}, d: { e: '' }, f: 0, g: false })).toEqual({ f: 0, g: false });
  });
});

describe('schema', () => {
  it('resolves references and nullable wrappers', () => {
    const conversation = unwrapNullable(root, prop('conversation'));
    expect(conversation.nullable).toBe(true);
    expect(Object.keys(conversation.schema.properties ?? {})).toContain('stages');
    expect(unwrapNullable(root, prop('greeting')).nullable).toBe(true);
    expect(kindOf(root, prop('state'))).toBe('map');
    expect(kindOf(root, prop('tools'))).toBe('array');
  });

  it('recognizes externally tagged variants', () => {
    const pred = { $ref: '#/definitions/Pred' };
    const labels = variants(root, pred).map((v) => v.label);
    expect(labels).toEqual(expect.arrayContaining(['always', 'is_true', 'captured', 'all', 'not', 'eq']));
    expect(variants(root, pred)[variantOf(root, pred, { captured: ['a'] })]!.label).toBe('captured');
    expect(variants(root, pred)[variantOf(root, pred, 'always')]!.label).toBe('always');
    expect(variants(root, pred).find((v) => v.label === 'eq')!.make()).toEqual({ eq: ['', null] });
  });

  it('makes minimal defaults', () => {
    const stage = resolve(root, { $ref: '#/definitions/StageSpec' });
    expect(defaultFor(root, stage)).toEqual({ id: '' });
    expect(defaultFor(root, { $ref: '#/definitions/ToolSpec' })).toEqual({ name: '' });
  });
});

const flowSpec: Spec = {
  name: 'demo',
  flow: {
    steps: [
      { id: 'greet', posture: 'Say hello', done: { is_true: 'greeted' } },
      { id: 'ask', after: ['greet'], done: { captured: ['party', 'slot'] }, allow: ['check'] },
      { id: 'end', after: [{ step: 'ask', when: { done: 'ask' } }], terminal: true },
    ],
  },
};

const conversationSpec: Spec = {
  name: 'booking',
  conversation: {
    name: 'booking',
    stages: [
      { id: 'collect', collect: ['party'], next: [{ to: 'confirm', when: { captured: ['party'] } }], repair: { escalate_to: 'handoff' } },
      { id: 'confirm', commit: { tool: 'book', when: { is_true: 'ok' } }, next: [{ to: 'done', when: { called_ok: 'book' } }] },
      { id: 'done', terminal: true },
      { id: 'handoff', terminal: true },
    ],
    require: ['done'],
    overlays: [{ name: 'faq', stages: [{ id: 'answer', terminal: true }] }],
  },
};

describe('graph', () => {
  it('derives a flow graph', () => {
    const { nodes, edges } = graphOf(flowSpec);
    expect(modeOf(flowSpec)).toBe('flow');
    expect(nodes.map((n) => n.id)).toEqual(['greet', 'ask', 'end']);
    expect(nodes[1]!.done).toBe('captured(party, slot)');
    expect(edges.map((e) => [e.source, e.target, e.label])).toEqual([
      ['greet', 'ask', undefined],
      ['ask', 'end', 'done(ask)'],
    ]);
  });

  it('derives a conversation graph with transitions, escalation and overlays', () => {
    const { nodes, edges } = graphOf(conversationSpec);
    expect(nodes.find((n) => n.id === 'answer')!.kind).toBe('overlay');
    expect(nodes.find((n) => n.id === 'confirm')!.commit).toBe('book');
    expect(edges.map((e) => `${e.kind}:${e.source}->${e.target}`)).toEqual([
      'next:collect->confirm',
      'escalate:collect->handoff',
      'next:confirm->done',
    ]);
  });

  it('connects and disconnects in both modes', () => {
    const flow = connect(flowSpec, 'greet', 'end');
    expect(getIn(flow, ['flow', 'steps', 2, 'after'])).toEqual([{ step: 'ask', when: { done: 'ask' } }, 'greet']);
    const edge = graphOf(flow).edges.find((e) => e.source === 'greet' && e.target === 'end')!;
    expect(graphOf(disconnect(flow, edge)).edges).toHaveLength(2);

    const conv = connect(conversationSpec, 'confirm', 'handoff');
    expect(getIn(conv, ['conversation', 'stages', 1, 'next', 1])).toEqual({ to: 'handoff', when: 'always' });
    const escalate = graphOf(conv).edges.find((e) => e.kind === 'escalate')!;
    expect(getIn(disconnect(conv, escalate), ['conversation', 'stages', 0, 'repair'])).toEqual({});
  });

  it('renames a node everywhere it is referenced', () => {
    const conv = renameNode(conversationSpec, 'done', 'booked');
    expect(getIn(conv, ['conversation', 'require'])).toEqual(['booked']);
    expect(getIn(conv, ['conversation', 'stages', 1, 'next', 0, 'to'])).toBe('booked');
    const flow = renameNode(flowSpec, 'ask', 'collect');
    expect(getIn(flow, ['flow', 'steps', 2, 'after', 0])).toEqual({ step: 'collect', when: { done: 'collect' } });
    // A taken id is refused.
    expect(renameNode(flowSpec, 'ask', 'greet')).toBe(flowSpec);
  });

  it('removes a node and its references', () => {
    const conv = removeNode(conversationSpec, 'handoff');
    expect(graphOf(conv).edges.some((e) => e.target === 'handoff')).toBe(false);
    expect(nodePath(conv, 'handoff')).toBeUndefined();
    const flow = removeNode(flowSpec, 'greet');
    expect(getIn(flow, ['flow', 'steps', 0, 'after'])).toBeUndefined();
  });

  it('adds nodes with unique ids and finds their paths', () => {
    const { spec, id } = addNode(conversationSpec, 'confirm');
    expect(id).toBe('confirm_2');
    expect(nodePath(spec, 'confirm_2')).toEqual(['conversation', 'stages', 4]);
    expect(nodePath(conversationSpec, 'answer')).toEqual(['conversation', 'overlays', 0, 'stages', 0]);
  });

  it('converts a flow to a conversation', () => {
    const conv = toConversation(flowSpec);
    expect(modeOf(conv)).toBe('conversation');
    expect(conv.flow).toBeUndefined();
    expect(getIn(conv, ['conversation', 'stages', 0])).toEqual({
      id: 'greet',
      say: 'Say hello',
      done: { is_true: 'greeted' },
      next: [{ to: 'ask', when: { done: 'greet' } }],
    });
  });

  it('keeps a conditional edge\'s condition when converting', () => {
    const conv = toConversation(flowSpec);
    expect(getIn(conv, ['conversation', 'stages', 1, 'next'])).toEqual([{ to: 'end', when: { all: [{ done: 'ask' }, { done: 'ask' }] } }]);
  });

  it('summarizes guards', () => {
    expect(guardSummary({ all: [{ is_true: 'a' }, { not: { is_set: 'b' } }] })).toBe('all(is_true(a), not(is_set(b)))');
    expect(guardSummary({ eq: ['tier', 'gold'] })).toBe('tier = "gold"');
  });
});
