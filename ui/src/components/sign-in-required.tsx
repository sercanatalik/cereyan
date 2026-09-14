import { useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
import { Modal } from "@/components/ui/modal";

export const SIGN_IN_REQUIRED_EVENT = "cereyan:sign-in-required";

/** Fired by the API client on a 401 from a server whose authenticator checks sign-in. */
export function announceSignInRequired(loginUrl: string | null) {
  window.dispatchEvent(new CustomEvent(SIGN_IN_REQUIRED_EVENT, { detail: { loginUrl } }));
}

/**
 * Shown instead of the token prompt when the identity provider signs users in:
 * there is nothing to type here, only somewhere to go and a way back.
 */
export function SignInRequired({ onRetry }: { onRetry?: () => void }) {
  const [open, setOpen] = useState(false);
  const [loginUrl, setLoginUrl] = useState<string | null>(null);
  useEffect(() => {
    const listener = (e: Event) => {
      const url = (e as CustomEvent).detail?.loginUrl;
      setLoginUrl(typeof url === "string" ? url : null);
      setOpen(true);
    };
    window.addEventListener(SIGN_IN_REQUIRED_EVENT, listener);
    return () => window.removeEventListener(SIGN_IN_REQUIRED_EVENT, listener);
  }, []);
  const retry = onRetry ?? (() => window.location.reload());
  return (
    <Modal
      open={open}
      onClose={() => setOpen(false)}
      title="Not signed in"
      footer={
        <>
          <Button variant="outline" onClick={retry} data-testid="sign-in-retry">
            Try again
          </Button>
          {loginUrl ? (
            <Button asChild>
              <a href={loginUrl} data-testid="sign-in-link">
                Sign in
              </a>
            </Button>
          ) : null}
        </>
      }
    >
      <p className="text-sm text-muted-foreground">
        {loginUrl
          ? "This server checks your sign-in with your identity provider. Sign in, then try again."
          : "This server checks your sign-in with your identity provider. Sign in to it in this browser, then try again."}
      </p>
    </Modal>
  );
}
