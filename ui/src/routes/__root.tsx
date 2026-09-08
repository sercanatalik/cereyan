import type { QueryClient } from "@tanstack/react-query";
import { createRootRouteWithContext, Outlet } from "@tanstack/react-router";
import { Shell } from "@/components/shell";

export const Route = createRootRouteWithContext<{ queryClient: QueryClient }>()({
  component: () => (
    <Shell>
      <Outlet />
    </Shell>
  ),
});
