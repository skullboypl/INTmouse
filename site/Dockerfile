# CapRover: kontekst builda to katalog site/ (captain-definition wskazuje ten plik).
# Brak bazy danych i woluminow. Strona jest statyczna z cache; jedyne zapytanie na
# zewnatrz to lista wydan z GitHuba (raz na godzine).
FROM node:24-slim AS builder
WORKDIR /app
COPY package.json package-lock.json ./
RUN npm ci
COPY . ./
RUN mkdir -p public && npm run build

FROM node:24-slim
WORKDIR /app
ENV NODE_ENV=production \
    PORT=3000 \
    HOSTNAME=0.0.0.0
COPY --from=builder /app/.next/standalone ./
COPY --from=builder /app/.next/static ./.next/static
COPY --from=builder /app/public ./public
EXPOSE 3000
CMD ["node", "server.js"]
