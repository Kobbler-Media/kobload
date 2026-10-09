FROM node:26-alpine AS deps

RUN apk add --no-cache git openssh-client

# Mirror the dev-box two-level layout (kob/kobload/web and kob/koblibs) so the
# "file:../../koblibs" path in package.json resolves for npm. `npm install`
# (rather than `npm ci`) is used because platform-specific optional deps
# (lightningcss, etc.) aren't fully resolved in the lockfile.
COPY --from=koblibs . /src/koblibs
WORKDIR /src/mono/web
COPY package.json package-lock.json ./
RUN --mount=type=cache,target=/root/.npm npm install

FROM deps AS dev

COPY . .
COPY docker-entrypoint.sh /usr/local/bin/
RUN chmod +x /usr/local/bin/docker-entrypoint.sh
EXPOSE 5173
ENTRYPOINT ["docker-entrypoint.sh"]
CMD ["npm", "run", "dev", "--", "--host", "0.0.0.0"]

FROM deps AS builder

COPY . .
ARG VITE_API_BASE_URL
ENV VITE_API_BASE_URL=${VITE_API_BASE_URL}
RUN npm run build

FROM nginx:1.31.3-alpine AS prod
COPY nginx.conf /etc/nginx/conf.d/default.conf
COPY --from=builder /src/mono/web/build /usr/share/nginx/html
EXPOSE 80
