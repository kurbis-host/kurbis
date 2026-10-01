import { expect, test } from "@playwright/test";

const expectedHeaders = {
  "content-security-policy": /default-src 'self'.*object-src 'none'.*frame-ancestors 'none'/,
  "referrer-policy": "strict-origin-when-cross-origin",
  "x-content-type-options": "nosniff",
  "x-frame-options": "DENY",
  "permissions-policy": "camera=(), microphone=(), geolocation=()",
};

test("public routes apply browser security headers", async ({ page }) => {
  for (const route of ["/", "/login"]) {
    const response = await page.goto(route);
    expect(response, `${route} should return an HTTP response`).not.toBeNull();
    const headers = response!.headers();
    for (const [name, value] of Object.entries(expectedHeaders)) {
      expect(headers[name], `${route} ${name}`).toMatch(value);
    }
    if (process.env.E2E_NEXT_MODE === "production") {
      expect(headers["strict-transport-security"]).toBe("max-age=31536000");
    } else {
      expect(headers["strict-transport-security"]).toBeUndefined();
    }
  }
});
