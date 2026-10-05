import { Button } from "@/components/ui/button";

/**
 * The footer of a keyset-paged list: where the page sits in the list, and
 * Previous / Next. The caller keeps the cursor history.
 */
export function PageFooter({
  from,
  count,
  noun,
  hasPrev,
  hasNext,
  onPrev,
  onNext,
  className,
}: {
  from: number;
  count: number;
  noun: string;
  hasPrev: boolean;
  hasNext: boolean;
  onPrev: () => void;
  onNext: () => void;
  className?: string;
}) {
  return (
    <div
      className={className ?? "flex items-center justify-between border-t px-4 py-2.5"}
      data-testid="page-footer"
    >
      <span className="text-xs tabular-nums text-muted-foreground">
        {count === 0 ? `No ${noun}` : `${from}–${from + count - 1}${hasNext ? " of more" : ""} ${noun}`}
      </span>
      <div className="flex gap-1.5">
        <Button variant="outline" size="sm" disabled={!hasPrev} onClick={onPrev}>
          Previous
        </Button>
        <Button variant="outline" size="sm" disabled={!hasNext} onClick={onNext}>
          Next
        </Button>
      </div>
    </div>
  );
}
