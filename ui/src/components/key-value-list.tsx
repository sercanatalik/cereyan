/** Labelled values in two aligned columns; a missing value reads as a dash. */
export function KeyValueList({ items }: { items: { label: string; value: React.ReactNode }[] }) {
  return (
    <dl className="grid grid-cols-[max-content_1fr] gap-x-6 gap-y-2 text-sm">
      {items.flatMap(({ label, value }) => [
        <dt key={`${label}:k`} className="text-muted-foreground">
          {label}
        </dt>,
        <dd key={`${label}:v`} className="min-w-0 break-words font-mono text-xs leading-5">
          {value ?? "-"}
        </dd>,
      ])}
    </dl>
  );
}
