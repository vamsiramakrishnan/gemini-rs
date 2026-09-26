// The whole document as JSON. Edits apply as soon as the text parses; the
// form and canvas views follow, and so does undo.

import { useEffect, useRef, useState } from 'react';
import { EditorState } from '@codemirror/state';
import { EditorView, keymap, lineNumbers, highlightActiveLine } from '@codemirror/view';
import { defaultKeymap, history, historyKeymap, indentWithTab } from '@codemirror/commands';
import { json, jsonParseLinter } from '@codemirror/lang-json';
import { linter, lintGutter } from '@codemirror/lint';
import { bracketMatching, foldGutter, indentOnInput, syntaxHighlighting, defaultHighlightStyle } from '@codemirror/language';
import { searchKeymap, highlightSelectionMatches } from '@codemirror/search';
import { closeBrackets } from '@codemirror/autocomplete';
import { useStudio } from '../store';
import type { Spec } from '../spec/graph';

export function JsonEditor() {
  const host = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView | null>(null);
  const spec = useStudio((s) => s.spec);
  const [error, setError] = useState<string | null>(null);
  const applying = useRef(false);

  useEffect(() => {
    if (!host.current) return;
    const editor = new EditorView({
      parent: host.current,
      state: EditorState.create({
        doc: JSON.stringify(useStudio.getState().spec, null, 2),
        extensions: [
          lineNumbers(),
          foldGutter(),
          history(),
          indentOnInput(),
          bracketMatching(),
          closeBrackets(),
          highlightActiveLine(),
          highlightSelectionMatches(),
          syntaxHighlighting(defaultHighlightStyle),
          json(),
          linter(jsonParseLinter()),
          lintGutter(),
          keymap.of([...defaultKeymap, ...historyKeymap, ...searchKeymap, indentWithTab]),
          EditorView.updateListener.of((update) => {
            if (!update.docChanged || applying.current) return;
            try {
              const parsed = JSON.parse(update.state.doc.toString()) as unknown;
              if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) {
                setError('The document must be a JSON object.');
                return;
              }
              setError(null);
              useStudio.getState().edit(parsed as Spec, 'json');
            } catch (err) {
              setError((err as Error).message);
            }
          }),
        ],
      }),
    });
    view.current = editor;
    return () => editor.destroy();
  }, []);

  // Follow edits made elsewhere (forms, canvas, undo) unless the text is
  // what produced them.
  useEffect(() => {
    const editor = view.current;
    if (!editor) return;
    let current: unknown;
    try {
      current = JSON.parse(editor.state.doc.toString());
    } catch {
      current = undefined;
    }
    if (JSON.stringify(current) === JSON.stringify(spec)) return;
    applying.current = true;
    editor.dispatch({ changes: { from: 0, to: editor.state.doc.length, insert: JSON.stringify(spec, null, 2) } });
    applying.current = false;
    setError(null);
  }, [spec]);

  return (
    <div className="json-editor">
      <div ref={host} className="cm-host" />
      {error && <div className="json-error">{error}</div>}
    </div>
  );
}
