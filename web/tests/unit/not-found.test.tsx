/** The page for an address with nothing at it: the game's lost connection, and the one way home. */

import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import NotFound, { metadata } from "../../app/not-found";

afterEach(cleanup);

describe("the missing page", () => {
  it("says plainly that nothing is here, and leads back to the landing page", () => {
    render(<NotFound />);
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("Connection Lost");
    expect(screen.getByText("There is no page at this address.")).toBeInTheDocument();
    expect(screen.getByText(/404/)).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Back to Title Screen" })).toHaveAttribute("href", "/");
    expect(metadata.title).toBe("Connection Lost");
  });
});
