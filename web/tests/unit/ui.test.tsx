import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { ConfirmDialog } from "../../components/ui";

describe("destructive confirmation", () => {
  it("requires an explicit dialog confirmation before invoking the action", async () => {
    const confirm = vi.fn(async () => {});
    const user = userEvent.setup();
    render(
      <ConfirmDialog
        danger
        title="Delete server?"
        description="This permanently deletes the server."
        confirmLabel="Delete server"
        onConfirm={confirm}
        trigger={<button>Delete server</button>}
      />,
    );

    await user.click(screen.getByRole("button", { name: "Delete server" }));
    expect(confirm).not.toHaveBeenCalled();
    expect(screen.getByRole("dialog")).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    expect(confirm).not.toHaveBeenCalled();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Delete server" }));
    await user.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Delete server" }));
    await waitFor(() => expect(confirm).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  });
});
