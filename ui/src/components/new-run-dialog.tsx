import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { useState } from "react";
import { ApiError, api, type Flow, unwrap } from "@/api/client";
import { RunForm } from "@/components/run-form";
import { Command, CommandEmpty, CommandInput, CommandItem, CommandList } from "@/components/ui/command";
import { Modal } from "@/components/ui/modal";

/** The top bar's New run: pick a flow, fill its parameters, start the run and open it. */
export function NewRunDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const navigate = useNavigate();
  const client = useQueryClient();
  const [flow, setFlow] = useState<Flow | null>(null);
  const [error, setError] = useState<string | null>(null);
  const flows = useQuery({
    queryKey: ["flows"],
    queryFn: async () => unwrap(await api.GET("/api/flows")),
    enabled: open,
  });
  const close = () => {
    setFlow(null);
    setError(null);
    onClose();
  };
  const run = useMutation({
    mutationFn: async ({ target, body }: { target: Flow; body: Record<string, unknown> }) =>
      unwrap(
        await api.POST("/api/flows/{id}/runs", {
          params: { path: { id: target.id } },
          body: { parameters: body as any, tags: [] },
        }),
      ),
    onSuccess: (created) => {
      close();
      client.invalidateQueries({ queryKey: ["runs"] });
      navigate({
        to: "/runs/$runId",
        params: { runId: String("conflict" in created ? created.run.id : created.id) },
      });
    },
    onError: (e) => setError(e instanceof ApiError ? e.message : String(e)),
  });
  return (
    <Modal
      open={open}
      onClose={close}
      title={flow ? `Run ${flow.project}/${flow.name}` : "New run"}
      description={flow ? "Parameters come from the flow signature." : "Pick the flow to run."}
    >
      {flow ? (
        <RunForm
          key={flow.id}
          schema={flow.parameter_schema}
          onSubmit={(body) => run.mutate({ target: flow, body })}
          busy={run.isPending}
          serverError={error}
        />
      ) : (
        <Command className="rounded-md border">
          <CommandInput placeholder="Find a flow" autoFocus />
          <CommandList>
            <CommandEmpty>No flow matches.</CommandEmpty>
            {(flows.data ?? [])
              .filter((f) => f.live !== false)
              .map((f) => (
                <CommandItem key={f.id} value={`${f.project}/${f.name}`} onSelect={() => setFlow(f)}>
                  <span className="text-muted-foreground">{f.project}/</span>
                  {f.name}
                </CommandItem>
              ))}
          </CommandList>
        </Command>
      )}
    </Modal>
  );
}
