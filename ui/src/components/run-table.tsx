import { Link } from "@tanstack/react-router";
import { type ColumnDef, flexRender, getCoreRowModel, useReactTable } from "@tanstack/react-table";
import type { Run } from "@/api/client";
import { StateBadge } from "@/components/ported/state-badge";
import { StateBar } from "@/components/state-bar";
import { Checkbox } from "@/components/ui/checkbox";
import { Table, Td, Th, Tr } from "@/components/ui/table";
import { cn, formatDuration, formatTime } from "@/lib/utils";

export function Tags({ tags }: { tags: string[] | null | undefined }) {
  if (!tags?.length) return null;
  return (
    <span className="flex flex-wrap gap-1">
      {tags.map((t) => (
        <span
          key={t}
          className="inline-flex h-5 items-center rounded-[5px] border bg-muted px-1.5 text-[11.5px] font-medium"
        >
          {t}
        </span>
      ))}
    </span>
  );
}

export function RunTable({
  runs,
  selected,
  onSelect,
  onSelectAll,
}: {
  runs: Run[];
  selected?: Set<number>;
  onSelect?: (id: number, checked: boolean) => void;
  onSelectAll?: (checked: boolean) => void;
}) {
  const allSelected = runs.length > 0 && runs.every((r) => selected?.has(r.id));
  const columns: ColumnDef<Run>[] = [
    ...(onSelect
      ? [
          {
            id: "select",
            header: () =>
              onSelectAll ? (
                <Checkbox
                  aria-label="Select all runs on this page"
                  checked={allSelected}
                  onCheckedChange={(c) => onSelectAll(c === true)}
                />
              ) : null,
            cell: ({ row }: { row: { original: Run } }) => (
              <Checkbox
                aria-label={`Select ${row.original.name}`}
                checked={selected?.has(row.original.id) ?? false}
                onCheckedChange={(c) => onSelect(row.original.id, c === true)}
              />
            ),
          } as ColumnDef<Run>,
        ]
      : []),
    {
      accessorKey: "state",
      header: "State",
      cell: ({ row }) => <StateBadge state={row.original.state} />,
    },
    {
      accessorKey: "name",
      header: "Name",
      cell: ({ row }) => (
        <Link
          to="/runs/$runId"
          params={{ runId: String(row.original.id) }}
          className="font-medium hover:underline"
        >
          {row.original.name}
        </Link>
      ),
    },
    {
      accessorKey: "flow_name",
      header: "Flow",
      cell: ({ row }) => (
        <span className="text-muted-foreground">
          {row.original.project}/{row.original.flow_name}
        </span>
      ),
    },
    {
      id: "tasks",
      header: "Tasks",
      cell: ({ row }) =>
        Object.keys(row.original.task_counts ?? {}).length ? (
          <StateBar counts={row.original.task_counts ?? {}} className="w-22" />
        ) : (
          <span className="text-muted-foreground">-</span>
        ),
    },
    {
      accessorKey: "start_time",
      header: "Started",
      cell: ({ row }) => (
        <span className="tabular-nums">{formatTime(row.original.start_time ?? row.original.created_at)}</span>
      ),
    },
    {
      accessorKey: "total_run_time",
      header: "Duration",
      cell: ({ row }) => <span className="tabular-nums">{formatDuration(row.original.total_run_time)}</span>,
    },
    {
      accessorKey: "tags",
      header: "Tags",
      cell: ({ row }) => <Tags tags={row.original.tags} />,
    },
  ];
  const table = useReactTable({ data: runs, columns, getCoreRowModel: getCoreRowModel() });
  return (
    <Table>
      <thead>
        {table.getHeaderGroups().map((hg) => (
          <tr key={hg.id}>
            {hg.headers.map((h) => (
              <Th key={h.id} className={cn(h.id === "select" && "w-8 pr-0")}>
                {flexRender(h.column.columnDef.header, h.getContext())}
              </Th>
            ))}
          </tr>
        ))}
      </thead>
      <tbody>
        {table.getRowModel().rows.map((row) => (
          <Tr
            key={row.id}
            data-run-id={row.original.id}
            data-state={selected?.has(row.original.id) ? "selected" : undefined}
          >
            {row.getVisibleCells().map((cell) => (
              <Td key={cell.id} className={cn(cell.column.id === "select" && "w-8 pr-0")}>
                {flexRender(cell.column.columnDef.cell, cell.getContext())}
              </Td>
            ))}
          </Tr>
        ))}
        {runs.length === 0 ? (
          <tr>
            <td colSpan={columns.length} className="px-3 py-8 text-center text-muted-foreground">
              No runs
            </td>
          </tr>
        ) : null}
      </tbody>
    </Table>
  );
}
