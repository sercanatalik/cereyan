/**
 * Flows and runs are organised in two levels: the project, and the group within
 * it. A flow's group is the one declared with `group=` in Python, else its
 * project — so a row whose resolved group equals its project is one of the
 * project's own rows rather than a nested section repeating the project's name.
 */

/** Anything the API returns carrying a project and a resolved group. */
export interface Grouped {
  project: string;
  group?: string | null;
}

/** The group of one row. The server resolves it; the fallback keeps types honest. */
export function groupOf(item: Grouped): string {
  return item.group ?? item.project;
}

export interface Group<T> {
  /** The group name, as shown in its header. */
  key: string;
  /** Open state is held under this, so one group name in two projects toggles separately. */
  openKey: string;
  /** The rows of this group, in the order they arrived. */
  items: T[];
}

export interface ProjectGroup<T> {
  /** The project name, as shown in its header. */
  key: string;
  openKey: string;
  /** Rows whose resolved group is the project itself. */
  rows: T[];
  /** The project's groups, alphabetical. */
  groups: Group<T>[];
  /** Every row in the project: its own rows and its groups'. */
  items: T[];
}

/**
 * Nest rows by project and then by group, alphabetical at both levels. The
 * order never depends on the rows themselves, so live updates cannot reshuffle
 * the page. Every row lands in exactly one place, so the project counts sum to
 * the total and there is no ungrouped bucket.
 */
export function nestByProject<T extends Grouped>(items: T[]): ProjectGroup<T>[] {
  const byProject = new Map<string, T[]>();
  for (const item of items) {
    const bucket = byProject.get(item.project);
    if (bucket) bucket.push(item);
    else byProject.set(item.project, [item]);
  }
  return Array.from(byProject.entries())
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([project, projectItems]) => {
      const rows: T[] = [];
      const byGroup = new Map<string, T[]>();
      for (const item of projectItems) {
        const key = groupOf(item);
        if (key === project) {
          rows.push(item);
          continue;
        }
        const bucket = byGroup.get(key);
        if (bucket) bucket.push(item);
        else byGroup.set(key, [item]);
      }
      const groups = Array.from(byGroup.entries())
        .sort(([a], [b]) => a.localeCompare(b))
        .map(([key, groupItems]) => ({
          key,
          openKey: `group:${project}/${key}`,
          items: groupItems,
        }));
      return {
        key: project,
        openKey: `project:${project}`,
        rows,
        groups,
        items: projectItems,
      };
    });
}

/** The distinct resolved groups of these rows, alphabetical, for a filter's options. */
export function groupOptions<T extends Grouped>(items: T[]): string[] {
  return Array.from(new Set(items.map(groupOf))).sort((a, b) => a.localeCompare(b));
}
