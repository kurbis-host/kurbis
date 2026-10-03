# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

## Users

_Inferred from `web/PLAN.md`._ Kurbis serves mixed-experience Minecraft friend groups and server owners. Casual users should be able to create, wake, join, and safely stop a server with little operational knowledge; experienced owners need progressively disclosed lifecycle, configuration, player, log, and backup controls.

## Product Purpose

_Inferred from `web/PLAN.md`._ Kurbis provides one managed Java + Bedrock server per account that can sleep when unused, wake when a player connects, restore its world from durable snapshots, and release infrastructure again after five idle minutes. Success means the hosting mechanism is understandable and trustworthy from both the public site and the authenticated control panel.

## Positioning

_Inferred from `web/PLAN.md`._ The distinctive mechanism is wake-on-connect Minecraft hosting backed by snapshot restoration and automatic idle shutdown, exposing the lifecycle clearly rather than presenting an always-on generic VPS panel.

## Operating Context

_Inferred from `web/PLAN.md`._ Users enter through OAuth, create one server, share its Java/Bedrock address, and manage lifecycle, online console commands, logs, settings, current players, independent whitelist and managed-operator lists, backups, restore operations, activity, and account state from a responsive browser UI.

## Capabilities and Constraints

_Inferred from `web/PLAN.md`._ One server per OAuth account; one region; Java + Bedrock; 2 GiB RAM; 5 GiB persistent storage; five-minute automatic sleep; no explicit player-count limit. V1 excludes payments, plugins/mod uploads, unrestricted file management, arbitrary binaries, and general administrative tooling. Authorized server-panel users can run one-line Minecraft commands only while a healthy runtime is already online and can manage a separate persistent operator roster; this does not grant arbitrary worker, gateway, or runtime API access. Browser traffic is same-origin through the Next.js frontend. The control plane exposes a typed `/api/v1` REST API plus authenticated durable SSE. English-only is sufficient for v1.

## Brand Commitments

_Inferred from `web/PLAN.md`._ Preserve the Kurbis name. The visual identity is original and Minecraft-adjacent through voxel geometry, material cues, pixel-aware details, and custom Kurbis iconography without copying Minecraft UI or textures.

## Evidence on Hand

The implementation and product specification live in `web/PLAN.md`; no testimonials, customer logos, pricing evidence, or third-party proof assets are supplied, so the product must not fabricate them.

## Product Principles

- Make lifecycle state and recovery legible at every stage.
- Keep casual hosting actions obvious while progressively disclosing expert controls.
- Treat snapshots, restores, and sleeping-state restrictions as safety-critical product behavior.
- Keep browser state derived from the typed control-plane API and durable server events.
- Prefer original Kurbis identity and accessible modern UX over imitation game chrome.

## Accessibility & Inclusion

_Inferred from `web/PLAN.md`._ Meet WCAG AA contrast, full keyboard operation, visible focus, correct ARIA/live-region behavior, minimum touch targets, reduced-motion support, and responsive layouts from 390px mobile through large desktop.
