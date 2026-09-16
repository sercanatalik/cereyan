import { Check, Copy } from "lucide-react";
import { useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

/** Copies text to the clipboard and reports `true` for a moment afterwards. */
function useCopied(text: string): [boolean, () => void] {
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), 1500);
    return () => clearTimeout(timer);
  }, [copied]);
  return [
    copied,
    () => {
      navigator.clipboard?.writeText(text);
      setCopied(true);
    },
  ];
}

/** A value as indented JSON with a copy button; `wrap` breaks long lines for narrow panels. */
export function JsonView({ value, wrap = false }: { value: unknown; wrap?: boolean }) {
  const text = JSON.stringify(value ?? null, null, 2);
  const [copied, copy] = useCopied(text);
  return (
    <div className="relative">
      <pre
        className={cn(
          "overflow-auto rounded-lg border bg-card p-3 font-mono text-xs leading-5",
          wrap && "whitespace-pre-wrap break-words",
        )}
      >
        {text}
      </pre>
      <Button
        variant="ghost"
        size="sm"
        className="absolute top-2 right-2"
        aria-label={copied ? "Copied" : "Copy JSON"}
        onClick={copy}
      >
        {copied ? <Check className="size-3.5" /> : <Copy className="size-3.5" />}
      </Button>
    </div>
  );
}
