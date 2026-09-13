import { cn } from "@/lib/utils";

/** Mutually exclusive options on a muted track, the chosen one lifted. */
export function Segmented({
  items,
  value,
  onChange,
  label,
  className,
}: {
  items: { value: string; label: React.ReactNode }[];
  value: string;
  onChange: (value: string) => void;
  label: string;
  className?: string;
}) {
  return (
    <div
      role="tablist"
      aria-label={label}
      className={cn("flex gap-0.5 rounded-lg bg-muted p-[3px]", className)}
    >
      {items.map((item) => (
        <button
          key={item.value}
          type="button"
          role="tab"
          aria-selected={item.value === value}
          onClick={() => onChange(item.value)}
          className={cn(
            "flex h-[26px] flex-1 cursor-pointer items-center justify-center rounded-md px-2 text-xs font-medium whitespace-nowrap",
            item.value === value
              ? "bg-card text-foreground shadow-xs"
              : "text-muted-foreground hover:text-foreground",
          )}
        >
          {item.label}
        </button>
      ))}
    </div>
  );
}
