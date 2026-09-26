// A form for any value, driven by the session spec's JSON Schema. Objects
// show their required fields and the optional ones already set; the rest
// are one "Add field" away, which keeps large sections readable.

import { useId, useMemo, useState, type ReactNode } from 'react';
import { isObject, type Json, type JsonObject } from '../spec/paths';
import {
  defaultFor,
  describe,
  kindOf,
  labelFor,
  unwrapNullable,
  variantOf,
  variants,
  type RootSchema,
  type Schema,
  type SchemaLike,
} from '../spec/schema';
import { LONG_TEXT, suggestionsFor, type Names } from '../spec/hints';

export interface FieldProps {
  root: RootSchema;
  schema: SchemaLike | undefined;
  value: Json | undefined;
  onChange: (value: Json | undefined) => void;
  names: Names;
  /** The property name this value sits under, for labels and hints. */
  name?: string;
  required?: boolean;
  /** Hide the label (the caller shows one). */
  bare?: boolean;
  depth?: number;
}

export function Field(props: FieldProps) {
  const { root, schema, value, onChange, name, required, bare } = props;
  const { schema: inner, nullable } = unwrapNullable(root, schema);
  const label = name ? labelFor(name) : undefined;
  const help = describe(inner.description);

  // An optional value that is absent: offer to set it.
  if ((value === undefined || value === null) && (nullable || !required) && kindOf(root, inner) !== 'boolean') {
    return (
      <div className="field field-unset">
        {!bare && label && <span className="field-label" title={help}>{label}</span>}
        <button type="button" className="link" onClick={() => onChange(defaultFor(root, inner) ?? '')}>
          set
        </button>
      </div>
    );
  }

  const control = <Control {...props} schema={inner} />;
  return (
    <div className={`field field-${kindOf(root, inner)}`}>
      {!bare && label && (
        <div className="field-head">
          <span className="field-label" title={help}>
            {label}
            {required && <span className="required">*</span>}
          </span>
          {!required && (
            <button type="button" className="icon" title="Remove" onClick={() => onChange(undefined)}>
              ×
            </button>
          )}
        </div>
      )}
      {control}
      {!bare && help && <p className="field-help">{help}</p>}
    </div>
  );
}

function Control(props: FieldProps & { schema: Schema }) {
  const { root, schema, value, onChange, names, name } = props;
  const listId = useId();
  const kind = kindOf(root, schema);
  switch (kind) {
    case 'string': {
      const suggestions = suggestionsFor(name, names);
      const text = typeof value === 'string' ? value : '';
      if (LONG_TEXT.has(name ?? '') || text.includes('\n')) {
        return <textarea value={text} rows={Math.min(8, Math.max(2, text.split('\n').length + 1))} onChange={(e) => onChange(e.target.value)} />;
      }
      return (
        <>
          <input type="text" value={text} list={suggestions ? listId : undefined} onChange={(e) => onChange(e.target.value)} />
          {suggestions && <Datalist id={listId} options={suggestions} />}
        </>
      );
    }
    case 'integer':
    case 'number':
      return (
        <input
          type="number"
          step={kind === 'integer' ? 1 : 'any'}
          value={typeof value === 'number' ? value : ''}
          onChange={(e) => onChange(e.target.value === '' ? undefined : Number(e.target.value))}
        />
      );
    case 'boolean':
      return <input type="checkbox" checked={value === true} onChange={(e) => onChange(e.target.checked ? true : undefined)} />;
    case 'enum':
      return (
        <select value={JSON.stringify(value ?? null)} onChange={(e) => onChange(JSON.parse(e.target.value) as Json)}>
          {(schema.enum ?? []).map((option) => (
            <option key={JSON.stringify(option)} value={JSON.stringify(option)}>
              {String(option)}
            </option>
          ))}
        </select>
      );
    case 'object':
      return <ObjectForm {...props} />;
    case 'map':
      return <MapForm {...props} />;
    case 'array':
      return <ArrayForm {...props} />;
    case 'tuple':
      return <TupleForm {...props} />;
    case 'variants':
      return <VariantForm {...props} />;
    default:
      return <JsonField value={value} onChange={onChange} />;
  }
}

function Datalist({ id, options }: { id: string; options: string[] }) {
  return (
    <datalist id={id}>
      {options.map((option) => (
        <option key={option} value={option} />
      ))}
    </datalist>
  );
}

export function ObjectForm(props: FieldProps & { schema: Schema; only?: string[] }) {
  const { root, schema, value, onChange, names, depth = 0, only } = props;
  const object: JsonObject = isObject(value) ? value : {};
  const properties = schema.properties ?? {};
  const required = new Set(schema.required ?? []);
  const keys = only ? only.filter((k) => k in properties) : ordered(Object.keys(properties), required);
  const shown = keys.filter((k) => required.has(k) || object[k] !== undefined);
  const addable = keys.filter((k) => !shown.includes(k));
  const update = (key: string, next: Json | undefined) => {
    const out = { ...object };
    if (next === undefined) delete out[key];
    else out[key] = next;
    onChange(out);
  };
  return (
    <div className="object" data-depth={depth}>
      {shown.map((key) => (
        <Field
          key={key}
          root={root}
          schema={properties[key]}
          value={object[key]}
          onChange={(next) => update(key, next)}
          names={names}
          name={key}
          required={required.has(key)}
          depth={depth + 1}
        />
      ))}
      {addable.length > 0 && (
        <AddField
          options={addable.map((k) => ({ key: k, help: describe(unwrapNullable(root, properties[k]).schema.description) }))}
          onAdd={(key) => update(key, defaultFor(root, unwrapNullable(root, properties[key]).schema) ?? '')}
        />
      )}
    </div>
  );
}

/** Identity first, then required fields, then the rest in schema order. */
const FIRST = ['id', 'name', 'to', 'when', 'description'];
function ordered(keys: string[], required: Set<string>): string[] {
  const rank = (k: string) => (FIRST.includes(k) ? FIRST.indexOf(k) : required.has(k) ? FIRST.length : FIRST.length + 1);
  return [...keys].sort((a, b) => rank(a) - rank(b));
}

function AddField({ options, onAdd }: { options: { key: string; help: string }[]; onAdd: (key: string) => void }) {
  return (
    <select className="add-field" value="" onChange={(e) => e.target.value && onAdd(e.target.value)}>
      <option value="">+ Add field…</option>
      {options.map((o) => (
        <option key={o.key} value={o.key} title={o.help}>
          {labelFor(o.key)}
        </option>
      ))}
    </select>
  );
}

function MapForm(props: FieldProps & { schema: Schema }) {
  const { root, schema, value, onChange, names, depth = 0, name } = props;
  const object: JsonObject = isObject(value) ? value : {};
  const [draft, setDraft] = useState('');
  const valueSchema = typeof schema.additionalProperties === 'object' ? schema.additionalProperties : true;
  const suggestions = name === 'set_state' ? names.stateKeys : undefined;
  const listId = useId();
  const add = () => {
    const key = draft.trim();
    if (!key || key in object) return;
    onChange({ ...object, [key]: defaultFor(root, valueSchema) });
    setDraft('');
  };
  return (
    <div className="map">
      {Object.entries(object).map(([key, inner]) => (
        <Card
          key={key}
          title={key}
          onRemove={() => {
            const out = { ...object };
            delete out[key];
            onChange(out);
          }}
        >
          <Field
            root={root}
            schema={valueSchema}
            value={inner}
            onChange={(next) => onChange({ ...object, [key]: next ?? null })}
            names={names}
            name={key}
            required
            bare
            depth={depth + 1}
          />
        </Card>
      ))}
      <div className="row">
        <input
          type="text"
          placeholder="new key"
          value={draft}
          list={suggestions ? listId : undefined}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => e.key === 'Enter' && (e.preventDefault(), add())}
        />
        {suggestions && <Datalist id={listId} options={suggestions} />}
        <button type="button" onClick={add} disabled={!draft.trim()}>
          Add
        </button>
      </div>
    </div>
  );
}

function ArrayForm(props: FieldProps & { schema: Schema }) {
  const { root, schema, value, onChange, names, depth = 0, name } = props;
  const items: Json[] = Array.isArray(value) ? value : [];
  const itemSchema = (schema.items as SchemaLike | undefined) ?? true;
  const itemKind = kindOf(root, itemSchema);
  if (itemKind === 'string' || (itemKind === 'enum' && unwrapNullable(root, itemSchema).schema.enum?.every((e) => typeof e === 'string'))) {
    return <TagList values={items.filter((i): i is string => typeof i === 'string')} onChange={onChange} suggestions={suggestionsFor(name, names) ?? unwrapNullable(root, itemSchema).schema.enum?.map(String)} />;
  }
  const move = (from: number, to: number) => {
    if (to < 0 || to >= items.length) return;
    const out = [...items];
    const [item] = out.splice(from, 1);
    out.splice(to, 0, item!);
    onChange(out);
  };
  return (
    <div className="array">
      {items.map((item, index) => (
        <Card
          key={index}
          title={itemTitle(item, index)}
          onRemove={() => onChange(items.filter((_, i) => i !== index))}
          onUp={index > 0 ? () => move(index, index - 1) : undefined}
          onDown={index < items.length - 1 ? () => move(index, index + 1) : undefined}
        >
          <Field
            root={root}
            schema={itemSchema}
            value={item}
            onChange={(next) => onChange(items.map((it, i) => (i === index ? (next ?? null) : it)))}
            names={names}
            name={name}
            required
            bare
            depth={depth + 1}
          />
        </Card>
      ))}
      <button type="button" className="add" onClick={() => onChange([...items, defaultFor(root, itemSchema)])}>
        + Add {name ? labelFor(name).replace(/s$/, '').toLowerCase() : 'item'}
      </button>
    </div>
  );
}

function itemTitle(item: Json, index: number): string {
  if (isObject(item)) {
    for (const key of ['id', 'name', 'to', 'key', 'kind', 'tool', 'slot']) {
      const v = item[key];
      if (typeof v === 'string' && v) return v;
    }
    const [first] = Object.keys(item);
    if (first && Object.keys(item).length === 1) return first;
  }
  if (typeof item === 'string') return item || `#${index + 1}`;
  return `#${index + 1}`;
}

function TupleForm(props: FieldProps & { schema: Schema }) {
  const { root, schema, value, onChange, names, depth = 0 } = props;
  const items = (schema.items as SchemaLike[]) ?? [];
  const values: Json[] = Array.isArray(value) ? value : items.map((s) => defaultFor(root, s));
  return (
    <div className="tuple">
      {items.map((itemSchema, index) => (
        <Field
          key={index}
          root={root}
          schema={itemSchema}
          value={values[index]}
          onChange={(next) => onChange(values.map((v, i) => (i === index ? (next ?? null) : v)))}
          names={names}
          name={index === 0 ? 'key' : 'value'}
          required
          depth={depth + 1}
        />
      ))}
    </div>
  );
}

function VariantForm(props: FieldProps & { schema: Schema }) {
  const { root, schema, value, onChange, names, depth = 0 } = props;
  const options = useMemo(() => variants(root, schema), [root, schema]);
  const current = variantOf(root, schema, value);
  const variant = options[current];
  const tagged = variant && isObject(value) && Object.keys(value).length === 1 && variant.label in value;
  return (
    <div className="variants">
      <select
        value={current}
        onChange={(e) => {
          const next = options[Number(e.target.value)];
          if (next) onChange(next.make());
        }}
        title={variant?.description ? describe(variant.description) : undefined}
      >
        {current < 0 && <option value={-1}>(custom)</option>}
        {options.map((option, index) => (
          <option key={`${option.label}-${index}`} value={index}>
            {option.label}
          </option>
        ))}
      </select>
      {variant && tagged && isObject(value) ? (
        <Field
          root={root}
          schema={variant.schema.properties?.[variant.label]}
          value={value[variant.label]}
          onChange={(next) => onChange({ [variant.label]: next ?? null })}
          names={names}
          name={variant.label}
          required
          bare
          depth={depth + 1}
        />
      ) : variant && kindOf(root, variant.schema) !== 'enum' ? (
        <Field root={root} schema={variant.schema} value={value} onChange={onChange} names={names} required bare depth={depth + 1} />
      ) : current < 0 ? (
        <JsonField value={value} onChange={onChange} />
      ) : null}
    </div>
  );
}

function TagList({ values, onChange, suggestions }: { values: string[]; onChange: (v: Json) => void; suggestions?: string[] }) {
  const [draft, setDraft] = useState('');
  const listId = useId();
  const add = (text: string) => {
    const tag = text.trim();
    if (!tag || values.includes(tag)) return;
    onChange([...values, tag]);
    setDraft('');
  };
  return (
    <div className="tags">
      {values.map((tag) => (
        <span className="tag" key={tag}>
          {tag}
          <button type="button" onClick={() => onChange(values.filter((t) => t !== tag))} aria-label={`Remove ${tag}`}>
            ×
          </button>
        </span>
      ))}
      <input
        type="text"
        value={draft}
        placeholder="add…"
        list={suggestions ? listId : undefined}
        onChange={(e) => {
          const text = e.target.value;
          // Picking a suggestion adds it straight away.
          if (suggestions?.includes(text)) add(text);
          else setDraft(text);
        }}
        onKeyDown={(e) => {
          if (e.key === 'Enter' || e.key === ',') {
            e.preventDefault();
            add(draft);
          } else if (e.key === 'Backspace' && !draft && values.length) {
            onChange(values.slice(0, -1));
          }
        }}
        onBlur={() => add(draft)}
      />
      {suggestions && <Datalist id={listId} options={suggestions.filter((s) => !values.includes(s))} />}
    </div>
  );
}

export function JsonField({ value, onChange }: { value: Json | undefined; onChange: (v: Json | undefined) => void }) {
  const [text, setText] = useState(() => JSON.stringify(value ?? null, null, 2));
  const [error, setError] = useState<string | null>(null);
  return (
    <div className="json-field">
      <textarea
        className="mono"
        value={text}
        rows={Math.min(10, text.split('\n').length + 1)}
        onChange={(e) => {
          setText(e.target.value);
          try {
            onChange(JSON.parse(e.target.value) as Json);
            setError(null);
          } catch (err) {
            setError((err as Error).message);
          }
        }}
      />
      {error && <p className="field-error">{error}</p>}
    </div>
  );
}

export function Card({
  title,
  children,
  onRemove,
  onUp,
  onDown,
}: {
  title: string;
  children: ReactNode;
  onRemove?: () => void;
  onUp?: () => void;
  onDown?: () => void;
}) {
  const [open, setOpen] = useState(true);
  return (
    <div className={`card ${open ? 'open' : ''}`}>
      <div className="card-head">
        <button type="button" className="card-toggle" onClick={() => setOpen(!open)}>
          {open ? '▾' : '▸'} {title}
        </button>
        <span className="card-actions">
          {onUp && (
            <button type="button" className="icon" title="Move up" onClick={onUp}>
              ↑
            </button>
          )}
          {onDown && (
            <button type="button" className="icon" title="Move down" onClick={onDown}>
              ↓
            </button>
          )}
          {onRemove && (
            <button type="button" className="icon" title="Remove" onClick={onRemove}>
              ×
            </button>
          )}
        </span>
      </div>
      {open && <div className="card-body">{children}</div>}
    </div>
  );
}
