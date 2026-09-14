import { act, fireEvent, render, screen } from "@testing-library/react";
import { handleUnauthorized } from "@/api/client";
import { announceSignInRequired, SIGN_IN_REQUIRED_EVENT, SignInRequired } from "./sign-in-required";
import { AUTH_REQUIRED_EVENT } from "./token-prompt";

test("sign-in panel links to the login URL and offers a retry", () => {
  let retried = 0;
  render(<SignInRequired onRetry={() => retried++} />);
  expect(screen.queryByRole("dialog")).toBeNull();
  act(() => announceSignInRequired("https://sso.example.com/login"));
  expect(screen.getByRole("dialog")).toHaveTextContent("Not signed in");
  expect(screen.getByTestId("sign-in-link")).toHaveAttribute("href", "https://sso.example.com/login");
  fireEvent.click(screen.getByTestId("sign-in-retry"));
  expect(retried).toBe(1);
  expect(document.cookie).not.toContain("cereyan_token");
});

test("sign-in panel without a login URL has no link", () => {
  render(<SignInRequired onRetry={() => {}} />);
  act(() => announceSignInRequired(null));
  expect(screen.getByRole("dialog")).toHaveTextContent("Not signed in");
  expect(screen.queryByTestId("sign-in-link")).toBeNull();
});

test("a 401 from an authenticator raises the sign-in panel, a token 401 the prompt", async () => {
  const seen: string[] = [];
  const onToken = () => seen.push("token");
  const onHook = (e: Event) => seen.push(`hook:${(e as CustomEvent).detail.loginUrl}`);
  window.addEventListener(AUTH_REQUIRED_EVENT, onToken);
  window.addEventListener(SIGN_IN_REQUIRED_EVENT, onHook);
  try {
    const request = new Request("http://localhost/api/runs");
    const reply = (auth: string, login_url: string | null) =>
      new Response(JSON.stringify({ error: "no", auth, login_url }), { status: 401 });
    await handleUnauthorized(request, reply("hook", "/login"));
    await handleUnauthorized(request, reply("token", null));
    await handleUnauthorized(request, new Response("not json", { status: 401 }));
  } finally {
    window.removeEventListener(AUTH_REQUIRED_EVENT, onToken);
    window.removeEventListener(SIGN_IN_REQUIRED_EVENT, onHook);
  }
  expect(seen).toEqual(["hook:/login", "token", "token"]);
});
