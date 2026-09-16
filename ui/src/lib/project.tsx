import { createContext, useCallback, useContext, useMemo, useState } from "react";

const KEY = "cereyan-project";
const GROUP_KEY = "cereyan-group";

interface ProjectScope {
  /** The project chosen in the top bar; empty means all projects. */
  project: string;
  /** The group within that project; empty means all of its groups. */
  group: string;
  /** Choose a project and a group together. Changing project alone clears the group. */
  setScope: (project: string, group?: string) => void;
}

const ProjectContext = createContext<ProjectScope>({ project: "", group: "", setScope: () => {} });

function readStored(key: string): string {
  try {
    return localStorage.getItem(key) ?? "";
  } catch {
    return "";
  }
}

function store(key: string, value: string) {
  try {
    if (value) localStorage.setItem(key, value);
    else localStorage.removeItem(key);
  } catch {}
}

/** Holds the project and group scope for the whole UI and mirrors it to localStorage. */
export function ProjectProvider({ children, initial }: { children: React.ReactNode; initial?: string }) {
  const [scope, setState] = useState(() => {
    const project = initial ?? readStored(KEY);
    // A stored group only means something under the project it was chosen in.
    return { project, group: initial === undefined && project ? readStored(GROUP_KEY) : "" };
  });
  const setScope = useCallback((project: string, group = "") => {
    const next = { project, group: project ? group : "" };
    setState(next);
    store(KEY, next.project);
    store(GROUP_KEY, next.group);
  }, []);
  const value = useMemo(() => ({ ...scope, setScope }), [scope, setScope]);
  return <ProjectContext.Provider value={value}>{children}</ProjectContext.Provider>;
}

/**
 * The effective project for a page: a `project` search parameter wins over the
 * scope chosen in the top bar. Returns `undefined` for "all projects" so it can
 * be passed straight to a query. The scope's group applies only while the
 * effective project is the scope's own.
 */
export function useProject(override?: string | null): {
  project: string | undefined;
  scope: string;
  group: string;
  setProject: (project: string) => void;
  setScope: (project: string, group?: string) => void;
} {
  const { project: scope, group, setScope } = useContext(ProjectContext);
  const effective = override || scope || undefined;
  const setProject = useCallback((project: string) => setScope(project), [setScope]);
  return { project: effective, scope, group: effective === scope ? group : "", setProject, setScope };
}
