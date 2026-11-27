import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

// B7: src/types.ts mirrors the Rust wire structs by hand (no codegen). The
// golden fixture src/types.contract.json is written against the real serde
// output (src-tauri/src/models.rs asserts the Rust side against it); this
// test pins the TS side of the same contract: every fixture field must
// exist in the mirroring interface, and extra TS fields must be on the
// explicitly-allowed live-fields list. Drift on either side turns red.

const FIXTURE = JSON.parse(
  readFileSync(join(import.meta.dirname, "..", "types.contract.json"), "utf8"),
) as Record<string, Record<string, unknown>>;
const TYPES = readFileSync(
  join(import.meta.dirname, "..", "types.ts"),
  "utf8",
);

function interfaceFields(name: string): Set<string> {
  const m = TYPES.match(new RegExp(`export interface ${name} \\{([^}]*)\\}`));
  assert.ok(m, `src/types.ts must declare interface ${name}`);
  const fields = new Set<string>();
  for (const line of m[1].split("\n")) {
    const f = line.trim().match(/^(\w+)\??:/);
    if (f) {
      fields.add(f[1]);
    }
  }
  return fields;
}

// DownloadJob carries live-only fields (speed/eta/byte counters) that the
// backend never serializes — they arrive via download://progress events.
const ALLOWED_TS_EXTRAS: Record<string, Set<string>> = new Map([
  ["DownloadJob", new Set(["speed", "eta", "downloaded_bytes", "total_bytes"])],
]);

for (const [struct, sample] of Object.entries(FIXTURE)) {
  if (struct === "UrlKind") {
    continue; // tagged union, checked separately below
  }
  test(`types.ts ${struct} mirrors the wire fixture exactly`, () => {
    const fields = interfaceFields(struct);
    const wire = new Set(Object.keys(sample));
    for (const wireField of wire) {
      assert.ok(
        fields.has(wireField),
        `${struct}.${wireField} exists on the wire but is missing from src/types.ts`,
      );
    }
    const extras = [...fields]
      .filter((f) => !wire.has(f))
      .filter((f) => !ALLOWED_TS_EXTRAS.get(struct)?.has(f));
    assert.deepEqual(
      extras,
      [],
      `${struct} has TS fields with no wire counterpart — declare them in the fixture or the allowed-extras list`,
    );
  });
}

test("UrlKind variants mirror the tagged enum one-for-one", () => {
  const start = TYPES.indexOf("export type UrlKind =");
  const end = TYPES.indexOf("export interface SpaceVideoItem", start);
  assert.ok(start >= 0 && end > start, "src/types.ts must declare the UrlKind union");
  const kinds = [...TYPES.slice(start, end).matchAll(/kind: "(\w+)"/g)].map(
    (m) => m[1],
  );
  const fixtureKinds = (FIXTURE["UrlKind"] as { kind: string }[]).map(
    (v) => v.kind,
  );
  assert.deepEqual(kinds, fixtureKinds);
});
