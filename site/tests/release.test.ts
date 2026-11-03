import assert from "node:assert/strict";
import { test } from "node:test";
import { pickRelease, summarize, toVersionDoc } from "../src/lib/release.ts";

const sha = "e991420ea8b09ec6c1006a971f850209bc2ea3098c60104da171a6de24538048";

const zip = {
  name: "IntMouse-0.2.0-beta.1-windows-x64.zip",
  browser_download_url: "https://github.com/o/r/releases/download/v0.2.0-beta.1/IntMouse-0.2.0-beta.1-windows-x64.zip",
  size: 4875570,
  digest: "sha256:0000000000000000000000000000000000000000000000000000000000000000",
};

const exe = {
  name: "inputshare.exe",
  browser_download_url: "https://github.com/o/r/releases/download/v0.2.0-beta.1/inputshare.exe",
  size: 11009856,
  digest: `sha256:${sha}`,
};

const betaRelease = {
  tag_name: "v0.2.0-beta.1",
  html_url: "https://github.com/o/r/releases/tag/v0.2.0-beta.1",
  prerelease: true,
  published_at: "2026-10-31T16:00:00Z",
  body: "## Beta 1\n\nIkona",
  assets: [zip, exe],
};

test("picks the newest non-draft release, pre-releases included", () => {
  const release = pickRelease([
    { tag_name: "v9", html_url: "https://x/9", draft: true, assets: [] },
    betaRelease,
    { tag_name: "v0.1.0", html_url: "https://x/1", assets: [] },
  ]);
  assert.equal(release?.tag, "v0.2.0-beta.1");
  assert.equal(release?.prerelease, true);
  assert.equal(release?.summary, "Beta 1");
});

test("zip and exe are told apart, and only the exe hash is the updater's hash", () => {
  const release = pickRelease([betaRelease]);
  assert.equal(release?.zip?.name, "IntMouse-0.2.0-beta.1-windows-x64.zip");
  assert.equal(release?.exe?.name, "inputshare.exe");
  assert.equal(release?.exe?.sha256, sha);
});

test("a release without a downloadable file still links to its page", () => {
  const release = pickRelease([{ tag_name: "v1", html_url: "https://x/1", assets: [{ name: "notes.txt", browser_download_url: "https://x/n" }] }]);
  assert.equal(release?.url, "https://x/1");
  assert.equal(release?.exe, null);
  assert.equal(release?.zip, null);
});

test("garbage and empty answers give no release", () => {
  assert.equal(pickRelease(null), null);
  assert.equal(pickRelease({ message: "Not Found" }), null);
  assert.equal(pickRelease([]), null);
  assert.equal(pickRelease([{ tag_name: "", html_url: "https://x" }, 42, null]), null);
});

test("summary is the first text line, capped", () => {
  assert.equal(summarize("\n\n- **Nowe** logo\nreszta"), "Nowe logo");
  assert.equal(summarize(undefined), "");
  assert.equal(summarize("a".repeat(300)).length, 198);
});

test("version.json carries what the app's updater reads", () => {
  const doc = toVersionDoc(pickRelease([betaRelease]));
  assert.deepEqual(doc, {
    version: "0.2.0-beta.1",
    tag: "v0.2.0-beta.1",
    url: "https://github.com/o/r/releases/tag/v0.2.0-beta.1",
    zip: zip.browser_download_url,
    exe: exe.browser_download_url,
    sha256: sha,
    notes: "Beta 1",
  });
});

test("version.json without a release has an empty version, which the app reads as up to date", () => {
  const doc = toVersionDoc(null);
  assert.equal(doc.version, "");
  assert.equal(doc.exe, null);
  assert.equal(doc.sha256, null);
});

test("a release with only a zip never offers an exe or a hash to install", () => {
  const doc = toVersionDoc(pickRelease([{ ...betaRelease, assets: [zip] }]));
  assert.equal(doc.exe, null);
  assert.equal(doc.sha256, null);
  assert.equal(doc.zip, zip.browser_download_url);
});
