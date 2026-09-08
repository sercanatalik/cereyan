import { useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Modal } from "@/components/ui/modal";

export const AUTH_REQUIRED_EVENT = "cereyan:auth-required";
const COOKIE = "cereyan_token";

/** Store the token as a strict same-site cookie scoped to the API path. */
export function storeToken(token: string) {
  // biome-ignore lint/suspicious/noDocumentCookie: the Cookie Store API is not available in every browser this UI targets
  document.cookie = `${COOKIE}=${encodeURIComponent(token)}; path=/api; SameSite=Strict`;
}

/** Fired by the API client on a 401; the prompt listens for it. */
export function announceAuthRequired(rejected: boolean) {
  window.dispatchEvent(new CustomEvent(AUTH_REQUIRED_EVENT, { detail: { rejected } }));
}

export function TokenPrompt({ onSubmit }: { onSubmit?: (token: string) => void }) {
  const [open, setOpen] = useState(false);
  const [rejected, setRejected] = useState(false);
  const [value, setValue] = useState("");
  useEffect(() => {
    const listener = (e: Event) => {
      setRejected(Boolean((e as CustomEvent).detail?.rejected));
      setOpen(true);
    };
    window.addEventListener(AUTH_REQUIRED_EVENT, listener);
    return () => window.removeEventListener(AUTH_REQUIRED_EVENT, listener);
  }, []);
  const submit = () => {
    const token = value.trim();
    if (!token) return;
    storeToken(token);
    setOpen(false);
    if (onSubmit) onSubmit(token);
    // The cookie now rides on every API call, including the event stream.
    else window.location.reload();
  };
  return (
    <Modal
      open={open}
      onClose={() => setOpen(false)}
      title="API token required"
      footer={
        <Button onClick={submit} disabled={!value.trim()} data-testid="token-submit">
          Continue
        </Button>
      }
    >
      <p className="mb-3 text-sm text-muted-foreground">
        {rejected
          ? "The server rejected the token. Enter the value from CEREYAN_TOKEN, --token, or [server] token."
          : "This server requires an API token. Enter the value from CEREYAN_TOKEN, --token, or [server] token."}
      </p>
      <Input
        type="password"
        value={value}
        placeholder="token"
        aria-label="API token"
        autoFocus
        onChange={(e) => setValue(e.target.value)}
        onKeyDown={(e) => e.key === "Enter" && submit()}
      />
    </Modal>
  );
}
