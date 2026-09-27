import { useEffect, useRef } from "react";
import { EditorState, Transaction, type Extension } from "@codemirror/state";
import { EditorView, keymap, lineNumbers, placeholder } from "@codemirror/view";
import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import { searchKeymap, highlightSelectionMatches } from "@codemirror/search";
import { closeBrackets, closeBracketsKeymap } from "@codemirror/autocomplete";
import {
  bracketMatching,
  foldGutter,
  foldKeymap,
  indentOnInput,
  syntaxHighlighting,
  HighlightStyle,
} from "@codemirror/language";
import { tags as t } from "@lezer/highlight";
import { json, jsonParseLinter } from "@codemirror/lang-json";
import { javascript } from "@codemirror/lang-javascript";
import { linter, lintGutter } from "@codemirror/lint";

export type EditorLanguage = "json" | "javascript" | "text" | "html" | "xml";

/** Themed against the app's CSS variables so both palettes work. */
const baseTheme = EditorView.theme({
  "&": { color: "var(--text)", backgroundColor: "var(--bg-input)", height: "100%" },
  ".cm-content": { caretColor: "var(--accent)", padding: "6px 0" },
  ".cm-cursor, .cm-dropCursor": { borderLeftColor: "var(--accent)" },
  "&.cm-focused .cm-selectionBackground, .cm-selectionBackground, .cm-content ::selection":
    { backgroundColor: "var(--accent-soft)" },
  ".cm-gutters": {
    backgroundColor: "var(--bg-sunken)",
    color: "var(--text-faint)",
    border: "none",
    borderRight: "1px solid var(--border)",
  },
  ".cm-activeLine": { backgroundColor: "transparent" },
  ".cm-activeLineGutter": { backgroundColor: "var(--bg-hover)" },
  ".cm-lineNumbers .cm-gutterElement": { padding: "0 6px 0 8px" },
  ".cm-foldPlaceholder": {
    backgroundColor: "var(--bg-active)",
    color: "var(--text-muted)",
    border: "none",
    padding: "0 6px",
  },
  ".cm-tooltip": {
    backgroundColor: "var(--bg-raised)",
    border: "1px solid var(--border)",
    color: "var(--text)",
  },
  ".cm-placeholder": { color: "var(--text-faint)" },
});

const highlight = HighlightStyle.define([
  { tag: [t.keyword, t.moduleKeyword, t.controlKeyword], color: "var(--method-patch)" },
  { tag: [t.string, t.special(t.string)], color: "var(--ok)" },
  { tag: [t.number, t.bool, t.null], color: "var(--method-post)" },
  { tag: [t.propertyName, t.definition(t.propertyName)], color: "var(--info)" },
  { tag: [t.comment], color: "var(--text-faint)", fontStyle: "italic" },
  { tag: [t.function(t.variableName), t.labelName], color: "var(--method-put)" },
  { tag: [t.operator, t.punctuation], color: "var(--text-muted)" },
  { tag: [t.typeName, t.className], color: "var(--warn)" },
  { tag: [t.invalid], color: "var(--danger)" },
]);

function languageExtensions(lang: EditorLanguage): Extension[] {
  switch (lang) {
    case "json":
      return [json(), linter(jsonParseLinter()), lintGutter()];
    case "javascript":
      return [javascript()];
    default:
      return [];
  }
}

/** What a caller can ask a mounted editor to do. */
export interface EditorActions {
  /** Replace the selection (or insert at the caret) and refocus. */
  insert: (text: string) => void;
}

interface Props {
  value: string;
  onChange?: (value: string) => void;
  language?: EditorLanguage;
  readOnly?: boolean;
  showLineNumbers?: boolean;
  placeholderText?: string;
  className?: string;
  /** Fires on Ctrl/Cmd+Enter, so editors can trigger Send. */
  onSubmit?: () => void;
  /**
   * Filled in with an insert handle while mounted.
   *
   * A ref rather than an imperative handle so a caller can hold one without
   * wrapping the editor, and so it is plainly optional.
   */
  actionsRef?: React.MutableRefObject<EditorActions | null>;
}

export function Editor({
  value,
  onChange,
  language = "text",
  readOnly = false,
  showLineNumbers = true,
  placeholderText,
  className,
  onSubmit,
  actionsRef,
}: Props) {
  const host = useRef<HTMLDivElement | null>(null);
  const view = useRef<EditorView | null>(null);
  const onChangeRef = useRef(onChange);
  const onSubmitRef = useRef(onSubmit);
  // The last text this editor reported upward. When it comes straight back
  // as `value`, nothing has to be done — and on a large body the
  // doc.toString() + compare it would otherwise cost is not free.
  const lastEmitted = useRef<string | null>(null);
  onChangeRef.current = onChange;
  onSubmitRef.current = onSubmit;

  useEffect(() => {
    if (!host.current) return;

    const extensions: Extension[] = [
      history(),
      bracketMatching(),
      closeBrackets(),
      indentOnInput(),
      highlightSelectionMatches(),
      syntaxHighlighting(highlight, { fallback: true }),
      baseTheme,
      EditorView.lineWrapping,
      keymap.of([
        {
          key: "Mod-Enter",
          run: () => {
            onSubmitRef.current?.();
            return true;
          },
        },
        ...closeBracketsKeymap,
        ...defaultKeymap,
        ...searchKeymap,
        ...historyKeymap,
        ...foldKeymap,
        indentWithTab,
      ]),
      ...languageExtensions(language),
      EditorState.readOnly.of(readOnly),
      EditorView.updateListener.of((u) => {
        if (u.docChanged) {
          const text = u.state.doc.toString();
          lastEmitted.current = text;
          onChangeRef.current?.(text);
        }
      }),
    ];
    if (showLineNumbers) extensions.push(lineNumbers(), foldGutter());
    if (placeholderText) extensions.push(placeholder(placeholderText));

    const v = new EditorView({
      state: EditorState.create({ doc: value, extensions }),
      parent: host.current,
    });
    view.current = v;
    if (actionsRef) {
      actionsRef.current = {
        insert: (text: string) => {
          const sel = v.state.selection.main;
          v.dispatch({
            changes: { from: sel.from, to: sel.to, insert: text },
            // Caret lands after what was inserted, so typing continues there
            // rather than jumping back to the start of a long token.
            selection: { anchor: sel.from + text.length },
          });
          v.focus();
        },
      };
    }
    return () => {
      v.destroy();
      view.current = null;
      if (actionsRef) actionsRef.current = null;
    };
    // Rebuilding on language/readOnly change is cheaper than reconfiguring.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [language, readOnly, showLineNumbers, placeholderText]);

  // Push external value changes in without clobbering local typing.
  useEffect(() => {
    const v = view.current;
    if (!v) return;
    if (value === lastEmitted.current) return;
    const current = v.state.doc.toString();
    if (current === value) return;
    lastEmitted.current = value;
    v.dispatch({
      changes: { from: 0, to: current.length, insert: value },
      // Not a user edit, so not an undo step: otherwise Ctrl+Z after a tab
      // switch restores the *previous tab's* text into this one.
      annotations: Transaction.addToHistory.of(false),
    });
  }, [value]);

  return <div ref={host} className={`cm-host ${className ?? ""}`} />;
}
