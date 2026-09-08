// Ported from Prefect's TagsInput (Apache 2.0); see NOTICE.
import { X } from "lucide-react";
import { useState } from "react";

export function TagInput({
  value,
  onChange,
  placeholder,
  icon,
}: {
  value: string[];
  onChange: (tags: string[]) => void;
  placeholder?: string;
  /** A leading icon, e.g. a filter glyph on the dashboard. */
  icon?: React.ReactNode;
}) {
  const [draft, setDraft] = useState("");
  const commit = () => {
    const tag = draft.trim();
    if (tag && !value.includes(tag)) onChange([...value, tag]);
    setDraft("");
  };
  return (
    <div className="flex h-8 min-w-48 flex-wrap items-center gap-1 rounded-md border border-input bg-transparent px-2 shadow-xs dark:bg-input/30">
      {icon ? <span className="mr-0.5 text-muted-foreground">{icon}</span> : null}
      {value.map((tag) => (
        <span
          key={tag}
          className="inline-flex h-5 items-center gap-1 rounded-[5px] border bg-muted px-1.5 text-[11.5px] font-medium"
        >
          {tag}
          <button
            type="button"
            aria-label={`Remove ${tag}`}
            onClick={() => onChange(value.filter((t) => t !== tag))}
          >
            <X className="h-3 w-3" />
          </button>
        </span>
      ))}
      <input
        className="min-w-16 flex-1 bg-transparent text-sm outline-none placeholder:text-muted-foreground"
        value={draft}
        placeholder={value.length ? "" : (placeholder ?? "Tags")}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === ",") {
            e.preventDefault();
            commit();
          } else if (e.key === "Backspace" && !draft && value.length) {
            onChange(value.slice(0, -1));
          }
        }}
        onBlur={commit}
      />
    </div>
  );
}
