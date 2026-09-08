import { act, fireEvent, render, screen } from "@testing-library/react";
import { announceAuthRequired, TokenPrompt } from "./token-prompt";

test("token prompt opens on 401 and stores the token", () => {
  const submitted: string[] = [];
  render(<TokenPrompt onSubmit={(t) => submitted.push(t)} />);
  expect(screen.queryByRole("dialog")).toBeNull();
  act(() => announceAuthRequired(false));
  expect(screen.getByRole("dialog")).toHaveTextContent("requires an API token");
  fireEvent.change(screen.getByLabelText("API token"), { target: { value: "s3cret" } });
  fireEvent.click(screen.getByTestId("token-submit"));
  expect(submitted).toEqual(["s3cret"]);
  expect(screen.queryByRole("dialog")).toBeNull();
  act(() => announceAuthRequired(true));
  expect(screen.getByRole("dialog")).toHaveTextContent("rejected the token");
});
