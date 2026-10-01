FROM node:22-bookworm-slim AS base
WORKDIR /app
RUN corepack enable && corepack prepare pnpm@11.2.2 --activate

FROM base AS dependencies
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
RUN pnpm install --frozen-lockfile --ignore-scripts

FROM dependencies AS build
COPY . .
ARG CONTROL_PLANE_INTERNAL_URL=http://control-plane:8080
ENV CONTROL_PLANE_INTERNAL_URL=$CONTROL_PLANE_INTERNAL_URL
ENV NEXT_TELEMETRY_DISABLED=1
RUN pnpm run build

FROM base AS runtime
ENV NODE_ENV=production
ENV NEXT_TELEMETRY_DISABLED=1
ENV HOSTNAME=0.0.0.0
ENV PORT=3000
COPY --chown=node:node --from=dependencies /app/node_modules ./node_modules
COPY --chown=node:node --from=build /app/.next ./.next
COPY --chown=node:node --from=build /app/next.config.ts ./next.config.ts
COPY --chown=node:node --from=build /app/lib/control-plane.ts ./lib/control-plane.ts
COPY --chown=node:node package.json pnpm-lock.yaml pnpm-workspace.yaml ./
USER node
EXPOSE 3000
CMD ["pnpm", "run", "start"]
