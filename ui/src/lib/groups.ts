/**
 * Flows and runs are organised by group: the one declared with `group=` in
 * Python, else the project. The axis is flat, so a declared group may span
 * projects and merges with a project of the same name.
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
  /** The group name, and the key the open state is held under. */
  key: string;
  /** The rows of this group, in the order they arrived. */
  items: T[];
  /** The distinct projects these rows come from, sorted. */
  projects: string[];
  /** True when the group holds more than one project, so no source is shared. */
  spansProjects: boolean;
}

/**
 * Group rows by their resolved group, alphabetically by name. The order never
 * depends on the rows themselves, so live updates cannot reshuffle the page.
 */
export function groupBy<T extends Grouped>(items: T[]): Group<T>[] {
  const byKey = new Map<string, T[]>();
  for (const item of items) {
    const key = groupOf(item);
    const bucket = byKey.get(key);
    if (bucket) bucket.push(item);
    else byKey.set(key, [item]);
  }
  return Array.from(byKey.entries())
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([key, groupItems]) => {
      const projects = Array.from(new Set(groupItems.map((i) => i.project))).sort();
      return { key, items: groupItems, projects, spansProjects: projects.length > 1 };
    });
}
