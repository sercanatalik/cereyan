/**
 * The URL path this UI is served under, without a trailing slash: "" at the
 * root, "/cereyan" behind a base path. The server injects it into index.html
 * as `<base href="/cereyan/">`; under `vite dev` there is no tag and the UI
 * runs at the root. Read the attribute rather than `document.baseURI`, which
 * falls back to the page URL when there is no tag.
 */
export function basePath(doc: Document = document): string {
  const href = doc.querySelector("base")?.getAttribute("href") ?? "/";
  return href.replace(/\/+$/, "");
}
