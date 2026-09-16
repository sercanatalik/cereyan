import { X } from "lucide-react";
import { useState } from "react";

/**
 * A list of tags edited in place: Enter or a comma adds what was typed,
 * Backspace on an empty field removes the last tag, and leaving the field keeps
 * what was typed.
 */
export function TagInput({
  value,
  onChange,
  placeholder = "Tags",
  icon,
}: {
  value: string[];
  onChange: (tags: string[]) => void;
  placeholder?: string;
  /** A leading icon, e.g. a filter glyph on the dashboard. */
  icon?: React.ReactNode;
}) {
  const [text, setText] = useState("");
  const add = () => {
    const tag = text.trim();
    setText("");
    if (tag && !value.includes(tag)) onChange([...value, tag]);
  };
  const onKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter" || e.key === ",") {
      e.preventDefault();
      add();
    } else if (e.key === "Backspace" && text === "" && value.length > 0) {
      onChange(value.slice(0, -1));
    }
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
            <X className="size-3" />
          </button>
        </span>
      ))}
      <input
        className="min-w-16 flex-1 bg-transparent text-sm outline-none placeholder:text-muted-foreground"
        value={text}
        placeholder={value.length ? undefined : placeholder}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={onKeyDown}
        onBlur={add}
      />
    </div>
  );
}
