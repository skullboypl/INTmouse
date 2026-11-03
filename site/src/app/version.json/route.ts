import { latestRelease, toVersionDoc } from "@/lib/release";

// Cached mirror of the newest GitHub release, read by the app's updater. Next needs a literal
// here, so keep it equal to RELEASE_REVALIDATE_SECONDS.
export const revalidate = 3600;

export async function GET() {
  const doc = toVersionDoc(await latestRelease());
  return Response.json(doc, {
    headers: { "Cache-Control": "public, s-maxage=3600, stale-while-revalidate=86400" },
  });
}
