import { create } from "zustand";
import { listProjects, loadProject, opInvoke, setActiveProject } from "@/ipc";
import type { Project } from "@/ipc/schemas";

type ProjectState = {
  projects: Project[];
  activeProjectId: string;
  loading: boolean;
  hydrate: (projects: Project[]) => void;
  load: () => Promise<void>;
  switchProject: (projectId: string) => Promise<Project>;
  /**
   * Rename through `project.rename`, then reload the list so every view sees
   * it. Rejects only when the rename itself fails.
   */
  renameProject: (projectId: string, name: string) => Promise<void>;
};

export const useProjectStore = create<ProjectState>((set, get) => ({
  projects: [],
  activeProjectId: "default",
  loading: false,
  hydrate: (projects) => {
    const active = get().activeProjectId;
    set({
      projects,
      activeProjectId: projects.some((p) => p.id === active)
        ? active
        : (projects[0]?.id ?? "default"),
    });
  },
  load: async () => {
    set({ loading: true });
    try {
      set({ projects: await listProjects() });
    } finally {
      set({ loading: false });
    }
  },
  switchProject: async (projectId) => {
    const project = await setActiveProject(projectId);
    set({ activeProjectId: project.id });
    await loadProject(project.id);
    return project;
  },
  renameProject: async (projectId, name) => {
    await opInvoke(projectId, "project.rename", { name });
    // The rename is saved; a failed list reload must not report it as failed.
    // The activity record for the rename triggers another reload as well.
    await get()
      .load()
      .catch(() => {});
  },
}));
