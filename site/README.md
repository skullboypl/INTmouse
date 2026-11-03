# INTmouse site (intmouse.vxh.pl)

One static Next.js page: what the program is, that it is free and open source, and a download link.
**No login, no admin panel, no database, no payments.**

The page copy is Polish. Titles, the meta description and Open Graph are English, with a Polish
summary in the structured data (`site/src/app/layout.tsx`).

## How the cache works

- The page is static (`export const revalidate = 3600`) and refreshes in the background at most once an hour.
  The response sends `Cache-Control: s-maxage=3600, stale-while-revalidate`.
- The only outside data is the release list from the GitHub API (`src/lib/release.ts`), cached for the same hour.
  If the repository does not exist, GitHub does not answer, or the rate limit runs out, the page shows a
  link to the code instead of a download button. Nothing breaks.
- The download banner uses the newest release (including a beta): the `.zip` (or the `.exe` if there is no zip)
  and the file’s SHA-256 when GitHub provides it.
- `GET /version.json` (`src/app/version.json/route.ts`) is that same cache in the shape the program reads, as in
  LiveDelay: `{version, tag, url, zip, exe, sha256, notes}`. The program fetches it a few seconds after start and
  every 12 hours. `exe` and `sha256` come from the release’s `.exe` asset (the hash from GitHub’s `digest` field).
  With no `.exe` asset the program only announces the release and opens its page; it installs nothing. With no
  release at all, `version` is empty, which the program treats as “you are up to date”.

## Run it

```bash
npm install
npm run dev          # http://localhost:3000
npm run build && npm start
npm test             # release-parsing tests
npm run lint && npm run typecheck
```

Environment variables (all optional, see `.env.example`): `GITHUB_REPO` (default `skullboypl/intmouse`) and
`GITHUB_TOKEN` (only to get past the 60 requests per hour limit from one IP). The site address is
`src/lib/site.ts` (`SITE_URL`).

## Deploy on CapRover (intmouse.vxh.pl)

1. In CapRover, create an app (for example `intmouse`) and add the domain `intmouse.vxh.pl` (enable HTTPS).
2. No environment variables, no volumes, no database.
3. From `site/`: `caprover deploy` (uses `captain-definition` and `Dockerfile`).
4. DNS: an `intmouse` A or CNAME record pointing at the CapRover server.

The image uses `output: "standalone"`, so the container has no `node_modules`.
