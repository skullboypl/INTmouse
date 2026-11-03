import { REPO } from "./site.ts";

/** How long the page and the GitHub answer stay cached before they are refreshed. */
export const RELEASE_REVALIDATE_SECONDS = 3600;

export type ReleaseAsset = {
  name: string;
  url: string;
  size: number;
  sha256: string | null;
};

export type Release = {
  tag: string;
  url: string;
  prerelease: boolean;
  publishedAt: string | null;
  summary: string;
  /** Archive for people (`.zip`). */
  zip: ReleaseAsset | null;
  /** The program itself (`.exe`): what the in-app updater installs after checking its SHA-256. */
  exe: ReleaseAsset | null;
};

/** `/version.json`: the format the app's updater reads (same idea as LiveDelay's). */
export type VersionDoc = {
  version: string;
  tag: string;
  url: string;
  zip: string | null;
  exe: string | null;
  sha256: string | null;
  notes: string;
};

const isRecord = (v: unknown): v is Record<string, unknown> =>
  typeof v === "object" && v !== null && !Array.isArray(v);

const str = (v: unknown): string | null => (typeof v === "string" && v.length > 0 ? v : null);

function parseAsset(raw: unknown): ReleaseAsset | null {
  if (!isRecord(raw)) return null;
  const name = str(raw.name);
  const url = str(raw.browser_download_url);
  if (!name || !url) return null;
  const digest = str(raw.digest);
  return {
    name,
    url,
    size: typeof raw.size === "number" ? raw.size : 0,
    sha256: digest?.startsWith("sha256:") ? digest.slice("sha256:".length) : null,
  };
}

/** First non-empty line of the release notes, without markdown markers, capped in length. */
export function summarize(body: unknown): string {
  const first = String(body ?? "")
    .split(/\r?\n/)
    .map((l) => l.replace(/^(#{1,6}|[>*-])\s+/, "").replace(/[*`]/g, "").trim())
    .find((l) => l.length > 0);
  if (!first) return "";
  return first.length > 200 ? `${first.slice(0, 197)}…` : first;
}

/** Newest published release (pre-releases included: the first builds are betas). */
export function pickRelease(list: unknown): Release | null {
  if (!Array.isArray(list)) return null;
  for (const raw of list) {
    if (!isRecord(raw) || raw.draft === true) continue;
    const tag = str(raw.tag_name);
    const url = str(raw.html_url);
    if (!tag || !url) continue;
    const assets = (Array.isArray(raw.assets) ? raw.assets.map(parseAsset) : []).filter(
      (a): a is ReleaseAsset => a !== null,
    );
    return {
      tag,
      url,
      prerelease: raw.prerelease === true,
      publishedAt: str(raw.published_at),
      summary: summarize(raw.body),
      zip: assets.find((a) => /\.zip$/i.test(a.name)) ?? null,
      exe: assets.find((a) => /\.exe$/i.test(a.name)) ?? null,
    };
  }
  return null;
}

/** What the app reads. No release yet = empty `version`, which the app treats as "up to date". */
export function toVersionDoc(release: Release | null): VersionDoc {
  if (!release) {
    return { version: "", tag: "", url: "", zip: null, exe: null, sha256: null, notes: "" };
  }
  return {
    version: release.tag.replace(/^v/i, ""),
    tag: release.tag,
    url: release.url,
    zip: release.zip?.url ?? null,
    exe: release.exe?.url ?? null,
    // The hash belongs to the exe only; without an exe the app just opens the release page.
    sha256: release.exe?.sha256 ?? null,
    notes: release.summary,
  };
}

/** Cached lookup; any failure (no repo yet, rate limit, offline build) means `null`. */
export async function latestRelease(): Promise<Release | null> {
  try {
    const token = process.env.GITHUB_TOKEN;
    const res = await fetch(`https://api.github.com/repos/${REPO}/releases?per_page=5`, {
      headers: {
        Accept: "application/vnd.github+json",
        "User-Agent": "intmouse-site",
        ...(token ? { Authorization: `Bearer ${token}` } : {}),
      },
      next: { revalidate: RELEASE_REVALIDATE_SECONDS },
      signal: AbortSignal.timeout(5000),
    });
    if (!res.ok) return null;
    return pickRelease(await res.json());
  } catch {
    return null;
  }
}
