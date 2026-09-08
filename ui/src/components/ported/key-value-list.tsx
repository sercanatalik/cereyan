// Ported from Prefect's KeyValue list (Apache 2.0); see NOTICE.
export function KeyValueList({ items }: { items: { label: string; value: React.ReactNode }[] }) {
  return (
    <dl className="grid grid-cols-[max-content_1fr] gap-x-6 gap-y-2 text-sm">
      {items.map((item) => (
        <div key={item.label} className="contents">
          <dt className="text-muted-foreground">{item.label}</dt>
          <dd className="min-w-0 break-words font-mono text-xs leading-5">{item.value ?? "-"}</dd>
        </div>
      ))}
    </dl>
  );
}
