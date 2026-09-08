import { createContext, useCallback, useContext, useMemo, useState } from "react";

const KEY = "cereyan-project";

interface ProjectScope {
  /** The project chosen in the top bar; empty means all projects. */
  project: string;
  setProject: (project: string) => void;
}

const ProjectContext = createContext<ProjectScope>({ project: "", setProject: () => {} });

function readStored(): string {
  try {
    return localStorage.getItem(KEY) ?? "";
  } catch {
    return "";
  }
}

/** Holds the project scope for the whole UI and mirrors it to localStorage. */
export function ProjectProvider({ children, initial }: { children: React.ReactNode; initial?: string }) {
  const [project, setState] = useState(() => initial ?? readStored());
  const setProject = useCallback((next: string) => {
    setState(next);
    try {
      if (next) localStorage.setItem(KEY, next);
      else localStorage.removeItem(KEY);
    } catch {}
  }, []);
  const value = useMemo(() => ({ project, setProject }), [project, setProject]);
  return <ProjectContext.Provider value={value}>{children}</ProjectContext.Provider>;
}

/**
 * The effective project for a page: a `project` search parameter wins over the
 * scope chosen in the top bar. Returns `undefined` for "all projects" so it can
 * be passed straight to a query.
 */
export function useProject(override?: string | null): {
  project: string | undefined;
  scope: string;
  setProject: (project: string) => void;
} {
  const { project: scope, setProject } = useContext(ProjectContext);
  const effective = override || scope || undefined;
  return { project: effective, scope, setProject };
}
