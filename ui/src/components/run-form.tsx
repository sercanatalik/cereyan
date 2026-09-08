// A form generated from a flow's parameter JSON schema, with a JSON toggle.
import { useMemo, useState } from "react";
import { Button } from "@/components/ui/button";
import { Input, Select, Textarea } from "@/components/ui/input";

type Prop = Record<string, any>;

export function defaultsFromSchema(schema: any): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  const props: Record<string, Prop> = schema?.properties ?? {};
  for (const [name, prop] of Object.entries(props)) {
    out[name] = prop && "default" in prop ? prop.default : null;
  }
  return out;
}

function fieldKind(prop: Prop): { kind: string; inner: Prop; optional: boolean } {
  if (Array.isArray(prop?.anyOf)) {
    const nonNull = prop.anyOf.find((p: Prop) => p.type !== "null");
    return { ...fieldKind(nonNull ?? {}), optional: true };
  }
  if (Array.isArray(prop?.enum)) return { kind: "enum", inner: prop, optional: false };
  if (prop?.type === "string" && prop.format === "date")
    return { kind: "date", inner: prop, optional: false };
  if (prop?.type === "string" && prop.format === "date-time")
    return { kind: "datetime", inner: prop, optional: false };
  if (prop?.type === "object" && prop.properties) return { kind: "object", inner: prop, optional: false };
  return { kind: prop?.type ?? "json", inner: prop ?? {}, optional: false };
}

export function coerceValue(kind: string, raw: string): { value: unknown; error?: string } {
  if (raw === "") return { value: null };
  switch (kind) {
    case "integer": {
      const n = Number(raw);
      if (!Number.isInteger(n)) return { value: raw, error: "must be an integer" };
      return { value: n };
    }
    case "number": {
      const n = Number(raw);
      if (Number.isNaN(n)) return { value: raw, error: "must be a number" };
      return { value: n };
    }
    case "boolean":
      return { value: raw === "true" };
    case "date":
      if (!/^\d{4}-\d{2}-\d{2}$/.test(raw)) return { value: raw, error: "must be an ISO date" };
      return { value: raw };
    case "datetime":
      if (Number.isNaN(Date.parse(raw))) return { value: raw, error: "must be an ISO datetime" };
      return { value: raw.length === 16 ? `${raw}:00` : raw };
    case "array":
    case "object":
    case "json":
      try {
        return { value: JSON.parse(raw) };
      } catch {
        return { value: raw, error: "must be valid JSON" };
      }
    default:
      return { value: raw };
  }
}

function Field({
  name,
  prop,
  value,
  required,
  onChange,
}: {
  name: string;
  prop: Prop;
  value: unknown;
  required: boolean;
  onChange: (v: unknown, error?: string) => void;
}) {
  const { kind, inner, optional } = fieldKind(prop);
  const [error, setError] = useState<string | undefined>();
  const set = (raw: string) => {
    const { value: v, error: e } = coerceValue(kind, raw);
    setError(e);
    onChange(v, e);
  };
  const text =
    value === null || value === undefined
      ? ""
      : typeof value === "object"
        ? JSON.stringify(value)
        : String(value);
  const label = (
    <label className="text-xs text-muted-foreground" htmlFor={`param-${name}`}>
      {name}
      {required ? " *" : ""}
      {optional ? " (optional)" : ""}
    </label>
  );
  let control: React.ReactNode;
  if (kind === "enum") {
    control = (
      <Select id={`param-${name}`} value={text} onChange={(e) => set(e.target.value)} className="w-full">
        <option value="">-</option>
        {inner.enum.map((c: unknown) => (
          <option key={String(c)} value={String(c)}>
            {String(c)}
          </option>
        ))}
      </Select>
    );
  } else if (kind === "boolean") {
    control = (
      <Select id={`param-${name}`} value={text} onChange={(e) => set(e.target.value)} className="w-full">
        <option value="">-</option>
        <option value="true">true</option>
        <option value="false">false</option>
      </Select>
    );
  } else if (kind === "object") {
    control = (
      <div className="ml-3 space-y-2 border-l pl-3">
        {Object.entries<Prop>(inner.properties ?? {}).map(([sub, subProp]) => (
          <Field
            key={sub}
            name={`${name}.${sub}`}
            prop={subProp}
            value={(value as any)?.[sub] ?? null}
            required={(inner.required ?? []).includes(sub)}
            onChange={(v) => onChange({ ...((value as object) ?? {}), [sub]: v })}
          />
        ))}
      </div>
    );
  } else {
    const type = kind === "date" ? "date" : kind === "datetime" ? "datetime-local" : "text";
    control =
      kind === "array" || kind === "json" ? (
        <Textarea id={`param-${name}`} rows={2} value={text} onChange={(e) => set(e.target.value)} />
      ) : (
        <Input
          id={`param-${name}`}
          type={type}
          inputMode={kind === "integer" || kind === "number" ? "decimal" : undefined}
          value={text}
          onChange={(e) => set(e.target.value)}
        />
      );
  }
  return (
    <div className="space-y-1">
      {label}
      {control}
      {error ? <div className="text-xs text-red-600">{error}</div> : null}
    </div>
  );
}

export function RunForm({
  schema,
  initial,
  onSubmit,
  submitLabel = "Run",
  busy,
  serverError,
}: {
  schema: any;
  initial?: Record<string, unknown>;
  onSubmit: (values: Record<string, unknown>) => void;
  submitLabel?: string;
  busy?: boolean;
  serverError?: string | null;
}) {
  const [values, setValues] = useState<Record<string, unknown>>(initial ?? defaultsFromSchema(schema));
  const [errors, setErrors] = useState<Record<string, string | undefined>>({});
  const [jsonMode, setJsonMode] = useState(false);
  const [jsonText, setJsonText] = useState(() =>
    JSON.stringify(initial ?? defaultsFromSchema(schema), null, 2),
  );
  const [jsonError, setJsonError] = useState<string | null>(null);
  const props = useMemo(() => Object.entries<Prop>(schema?.properties ?? {}), [schema]);
  const required: string[] = schema?.required ?? [];

  const submit = () => {
    if (jsonMode) {
      try {
        const parsed = JSON.parse(jsonText);
        setJsonError(null);
        onSubmit(parsed);
      } catch {
        setJsonError("Parameters must be a JSON object");
      }
      return;
    }
    const missing = required.filter((r) => values[r] === null || values[r] === undefined);
    if (missing.length) {
      setErrors((e) => ({ ...e, ...Object.fromEntries(missing.map((m) => [m, "required"])) }));
      return;
    }
    if (Object.values(errors).some(Boolean)) return;
    const clean: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(values)) if (v !== null && v !== undefined) clean[k] = v;
    onSubmit(clean);
  };

  return (
    <div className="space-y-3" data-testid="run-form">
      <div className="flex items-center justify-between">
        <span className="text-xs text-muted-foreground">
          {props.length ? "Parameters" : "This flow takes no parameters"}
        </span>
        <button
          type="button"
          className="text-xs underline"
          onClick={() => {
            if (!jsonMode) setJsonText(JSON.stringify(values, null, 2));
            else {
              try {
                setValues(JSON.parse(jsonText));
                setJsonError(null);
              } catch {
                setJsonError("Parameters must be a JSON object");
                return;
              }
            }
            setJsonMode((m) => !m);
          }}
        >
          {jsonMode ? "Form" : "JSON"}
        </button>
      </div>
      {jsonMode ? (
        <Textarea
          rows={8}
          value={jsonText}
          onChange={(e) => setJsonText(e.target.value)}
          aria-label="Parameters JSON"
        />
      ) : (
        props.map(([name, prop]) => (
          <Field
            key={name}
            name={name}
            prop={prop}
            value={values[name]}
            required={required.includes(name)}
            onChange={(v, error) => {
              setValues((old) => ({ ...old, [name]: v }));
              setErrors((old) => ({ ...old, [name]: error }));
            }}
          />
        ))
      )}
      {jsonError ? <div className="text-xs text-red-600">{jsonError}</div> : null}
      {Object.entries(errors)
        .filter(([, e]) => e === "required")
        .map(([k]) => (
          <div key={k} className="text-xs text-red-600">
            {k} is required
          </div>
        ))}
      {serverError ? <div className="text-xs text-red-600">{serverError}</div> : null}
      <div className="flex justify-end">
        <Button onClick={submit} disabled={busy}>
          {submitLabel}
        </Button>
      </div>
    </div>
  );
}
