import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createRouter, RouterProvider } from "@tanstack/react-router";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./index.css";
import { TokenPrompt } from "./components/token-prompt";
import { LiveProvider } from "./lib/live";
import { ProjectProvider } from "./lib/project";
import { routeTree } from "./routeTree.gen";

const queryClient = new QueryClient({
  defaultOptions: { queries: { staleTime: 5_000, refetchOnWindowFocus: false, retry: 1 } },
});

const router = createRouter({ routeTree, context: { queryClient } });

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}

const root = document.getElementById("root");
if (root) {
  createRoot(root).render(
    <StrictMode>
      <QueryClientProvider client={queryClient}>
        <LiveProvider>
          <ProjectProvider>
            <RouterProvider router={router} />
            <TokenPrompt />
          </ProjectProvider>
        </LiveProvider>
      </QueryClientProvider>
    </StrictMode>,
  );
}
