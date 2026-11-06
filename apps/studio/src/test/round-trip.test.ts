import { describe, expect, it } from 'vitest';
import { isObject, prune, type Json } from '../spec/paths';
// Fixtures: the example flows shipped with the runtime's web app, imported
// from apps/gemini-adk-web-rs/static/examples/flows/. The Rust test
// apps/gemini-adk-web-rs/tests/studio_round_trip.rs loads the same files
// through SessionSpec and compares them with a copy of this `prune`.
import collections from '../../../gemini-adk-web-rs/static/examples/flows/collections.json';
import restaurant from '../../../gemini-adk-web-rs/static/examples/flows/restaurant.json';
import returnsDesk from '../../../gemini-adk-web-rs/static/examples/flows/returns-desk.json';
import supportAgent from '../../../gemini-adk-web-rs/static/examples/flows/support-agent.json';

/** Paths (and values) that `after` lacks but `before` has. */
function removed(before: Json, after: Json | undefined, path = ''): [string, Json][] {
  if (isObject(before)) {
    const out: [string, Json][] = [];
    for (const [k, v] of Object.entries(before)) {
      const next = isObject(after) ? after[k] : undefined;
      if (next === undefined) out.push([`${path}/${k}`, v]);
      else out.push(...removed(v, next, `${path}/${k}`));
    }
    return out;
  }
  if (Array.isArray(before) && Array.isArray(after)) {
    return before.flatMap((v, i) => removed(v, after[i], `${path}/${i}`));
  }
  return [];
}

const isEmpty = (v: Json) =>
  v === '' || (Array.isArray(v) && v.length === 0) || (isObject(v) && Object.keys(v).length === 0);

describe('runtime example documents and the Studio prune', () => {
  it('leaves returns-desk exactly as the runtime accepts it', () => {
    expect(prune(returnsDesk as unknown as Json)).toEqual(returnsDesk);
  });

  it.each([
    ['collections', collections],
    ['restaurant', restaurant],
    ['support-agent', supportAgent],
    ['returns-desk', returnsDesk],
  ])('%s: prune only drops empty values and is idempotent', (_name, doc) => {
    const pruned = prune(doc as unknown as Json) as Json;
    // Everything it removes is "" / [] / {}: the runtime fills these defaults
    // back in on load (see the allow-list in studio_round_trip.rs), so the
    // Studio loses no authored data.
    const lost = removed(doc as unknown as Json, pruned).filter(([, v]) => !isEmpty(v));
    expect(lost).toEqual([]);
    expect(prune(pruned)).toEqual(pruned);
  });
});
