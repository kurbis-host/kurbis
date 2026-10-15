/** The legal pages: there, linked to each other, and with nothing left to fill in. */

import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import ImpressumPage from "../../app/(marketing)/impressum/page";
import LoginPage from "../../app/(marketing)/login/page";
import PrivacyPage from "../../app/(marketing)/privacy/page";

afterEach(cleanup);

describe("legal pages", () => {
  it("names the provider and how to reach them", () => {
    render(<ImpressumPage />);
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("Impressum");
    expect(screen.getAllByRole("heading", { level: 2 }).map((heading) => heading.textContent)).toEqual([
      "Provider",
      "Contact",
      "Responsible for the content",
      "Consumer dispute resolution",
    ]);
    expect(screen.getByRole("link", { name: "privacy policy" })).toHaveAttribute("href", "/privacy");
    expect(screen.getByRole("link", { name: "Back to the world" })).toHaveAttribute("href", "/");
    expect(screen.getByText(/not an official Minecraft or PumpkinMC product/)).toBeInTheDocument();
  });

  it("says what is stored, why, and for how long", () => {
    render(<PrivacyPage />);
    expect(screen.getAllByRole("heading", { level: 2 }).map((heading) => heading.textContent)).toEqual([
      "Who is responsible",
      "Visiting the website",
      "Signing in",
      "Joining from Minecraft",
      "Your server",
      "The AI assistant",
      "Plugins",
      "Who else receives data",
      "Your rights",
    ]);
    expect(screen.getByText(/lasts 30 days/)).toBeInTheDocument();
    expect(screen.getByText(/Conversations are deleted after 30 days/)).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Impressum" })).toHaveAttribute("href", "/impressum");
  });

  it("leaves nothing for the operator to fill in", () => {
    for (const Page of [ImpressumPage, PrivacyPage]) {
      const { container, unmount } = render(<Page />);
      expect(container.querySelectorAll("mark.game-fill")).toHaveLength(0);
      // (A placeholder written as plain capitals, like "HOSTING PROVIDER", would slip past the marks.)
      expect(container.textContent).not.toMatch(/\b[A-Z]{3,}(?: [A-Z]{3,})+\b/);
      unmount();
    }
  });

  it("is linked from where people sign in", () => {
    render(<LoginPage />);
    expect(screen.getByRole("link", { name: "Privacy" })).toHaveAttribute("href", "/privacy");
    expect(screen.getByRole("link", { name: "Impressum" })).toHaveAttribute("href", "/impressum");
  });
});
