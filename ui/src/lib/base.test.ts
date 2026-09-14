import { afterEach, describe, expect, it } from "vitest";
import { tokenCookie } from "@/components/token-prompt";
import { basePath } from "./base";

function setBase(href: string) {
  const tag = document.createElement("base");
  tag.setAttribute("href", href);
  document.head.prepend(tag);
}

afterEach(() => {
  for (const tag of document.head.querySelectorAll("base")) tag.remove();
});

describe("basePath", () => {
  it("is the root without a base tag, as under vite dev", () => {
    expect(basePath()).toBe("");
  });

  it("is the root when the server injects /", () => {
    setBase("/");
    expect(basePath()).toBe("");
  });

  it("drops the trailing slash of an injected base path", () => {
    setBase("/cereyan/");
    expect(basePath()).toBe("/cereyan");
  });
});

describe("tokenCookie", () => {
  it("scopes the token to the API under the base path", () => {
    expect(tokenCookie("t k", "")).toBe("cereyan_token=t%20k; path=/api; SameSite=Strict");
    expect(tokenCookie("tk", "/cereyan")).toBe("cereyan_token=tk; path=/cereyan/api; SameSite=Strict");
  });
});
