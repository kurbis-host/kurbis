/* This file is generated from contracts/openapi.json. Do not edit it directly. */
import type { components as SharedComponents, webhooks as SharedWebhooks, $defs as SharedDefinitions } from "./generated/schemas";
import type { operations as Operations0, paths as Paths0 } from "./generated/auth";
import type { operations as Operations1, paths as Paths1 } from "./generated/account";
import type { operations as Operations2, paths as Paths2 } from "./generated/servers";
import type { operations as Operations3, paths as Paths3 } from "./generated/server-data";
import type { operations as Operations4, paths as Paths4 } from "./generated/server-stats";
import type { operations as Operations5, paths as Paths5 } from "./generated/server-console";
import type { operations as Operations6, paths as Paths6 } from "./generated/server-ai";

export interface paths extends Paths0, Paths1, Paths2, Paths3, Paths4, Paths5, Paths6 {}
export type components = SharedComponents;
export type operations = Operations0 & Operations1 & Operations2 & Operations3 & Operations4 & Operations5 & Operations6;
export type webhooks = SharedWebhooks;
export type $defs = SharedDefinitions;
