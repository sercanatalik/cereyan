import { Link } from "@tanstack/react-router";
import { type ColumnDef, flexRender, getCoreRowModel, type Row, useReactTable } from "@tanstack/react-table";
import type { ReactNode } from "react";
import type { Run } from "@/api/client";
import {
  GroupCount,
  GroupSection,
  GroupStateRollup,
  GroupTags,
  useGroupOpen,
} from "@/components/grouped-rows";
import { StateBadge } from "@/components/ported/state-badge";
import { StateBar } from "@/components/state-bar";
import { Checkbox } from "@/components/ui/checkbox";
import { Table, Td, Th, Tr } from "@/components/ui/table";
import { groupBy } from "@/lib/groups";
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
  grouped = false,
  searchActive = false,
}: {
  runs: Run[];
  selected?: Set<number>;
  onSelect?: (id: number, checked: boolean) => void;
  onSelectAll?: (checked: boolean) => void;
  /** Collapse the runs into groups. Off where the list is already one flow. */
  grouped?: boolean;
  searchActive?: boolean;
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
  const rows = table.getRowModel().rows;
  const renderRow = (row: (typeof rows)[number]) => (
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
  );
  const empty =
    runs.length === 0 ? (
      <tbody>
        <tr>
          <td colSpan={columns.length} className="px-3 py-8 text-center text-muted-foreground">
            No runs
          </td>
        </tr>
      </tbody>
    ) : null;
  const head = (
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
  );
  if (!grouped) {
    return (
      <Table>
        {head}
        <tbody>{rows.map(renderRow)}</tbody>
        {empty}
      </Table>
    );
  }
  return (
    <Table>
      {head}
      <GroupedRunBodies
        rows={rows}
        renderRow={renderRow}
        searchActive={searchActive}
        hasSelect={!!onSelect}
      />
      {empty}
    </Table>
  );
}

/**
 * The runs table in collapsible groups. The state rollup sits in the State
 * column and the group name in the Name column, so a collapsed group reads as
 * the same table zoomed out.
 */
function GroupedRunBodies({
  rows,
  renderRow,
  searchActive,
  hasSelect,
}: {
  rows: Row<Run>[];
  renderRow: (row: Row<Run>) => ReactNode;
  searchActive: boolean;
  hasSelect: boolean;
}) {
  const groups = groupBy(rows.map((r) => r.original));
  const open = useGroupOpen(groups, searchActive);
  const byId = new Map(rows.map((r) => [r.original.id, r]));
  return (
    <>
      {groups.map((group) => {
        const counts: Record<string, number> = {};
        for (const run of group.items) counts[run.state.type] = (counts[run.state.type] ?? 0) + 1;
        const tags = Array.from(new Set(group.items.flatMap((r) => r.tags ?? []))).sort();
        return (
          <GroupSection
            key={group.key}
            group={group}
            open={open.isOpen(group.key)}
            onOpenChange={(next) => open.toggle(group.key, next)}
            // Only worth a line when it says something the group name does not.
            identity={group.spansProjects ? group.projects.join(" \u00b7 ") : undefined}
            leading={
              <>
                {hasSelect ? <Td className="w-8 pr-0" /> : null}
                <Td>
                  <GroupStateRollup counts={counts} />
                </Td>
              </>
            }
            rollup={
              <>
                <Td className="text-xs">
                  <GroupCount shown={group.items.length} />
                </Td>
                <Td colSpan={3} />
                <Td>
                  <GroupTags tags={tags} />
                </Td>
              </>
            }
          >
            {group.items.map((run) => {
              const row = byId.get(run.id);
              return row ? renderRow(row) : null;
            })}
          </GroupSection>
        );
      })}
    </>
  );
}
