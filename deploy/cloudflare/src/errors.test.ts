// node --test src/errors.test.ts (Node strips the types itself)
import { test } from "node:test";
import assert from "node:assert/strict";
import { EMPTY, MAX_KEPT, WINDOW_MS, exposition, record } from "./errors.ts";

const line = (text: string, name: string) =>
  Number(text.split("\n").find((l) => l.startsWith(`${name} `))?.split(" ")[1]);

test("errors inside the window are counted, older ones only in the total", () => {
  const t0 = 1_000_000_000_000;
  let log = record(EMPTY, t0);
  log = record(log, t0 + 1000);
  log = record(log, t0 + WINDOW_MS + 500); // t0 is now out of the window
  const text = exposition(log, t0 + WINDOW_MS + 500);
  assert.equal(line(text, "homestead_edge_server_errors_total"), 3);
  assert.equal(line(text, "homestead_edge_server_errors_24h"), 2);
  // A day of quiet later, the gauge is back to zero; the counter isn't.
  const later = exposition(log, t0 + 3 * WINDOW_MS);
  assert.equal(line(later, "homestead_edge_server_errors_24h"), 0);
  assert.equal(line(later, "homestead_edge_server_errors_total"), 3);
});

test("a flood of errors keeps at most MAX_KEPT timestamps", () => {
  let log = EMPTY;
  for (let i = 0; i < MAX_KEPT + 50; i++) log = record(log, 5_000 + i);
  assert.equal(log.recent.length, MAX_KEPT);
  assert.equal(log.total, MAX_KEPT + 50);
  assert.equal(log.recent.at(-1), 5_000 + MAX_KEPT + 49);
});

test("the exposition is valid Prometheus text ending in a newline", () => {
  const text = exposition(record(EMPTY, 1), 2);
  assert.ok(text.endsWith("\n"));
  for (const l of text.trimEnd().split("\n")) {
    assert.match(l, /^(# (HELP|TYPE) homestead_edge_\w+ .+|homestead_edge_\w+ \d+)$/);
  }
});
