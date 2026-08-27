import { appendFileSync, renameSync, statSync } from "node:fs";

/**
 * #964: `bus/events.jsonl` had three producers and only one bounded its
 * writes. This is the shared, bounded appender both piing-side producers
 * (`organization-intercom.ts`) route through -- one
 * implementation, not one per file. Pi extensions are copied standalone into
 * each pi-home, so they must not import launcher-internal code; the helper
 * therefore lives here, beside them.
 *
 * #751/G5: the third producer and the constant this one had to stay equal to
 * (`org-log.ts`'s `ORG_JOURNAL_MAX_BYTES`) are DELETED -- ported to Rust,
 * where the journal is a 48h rolling SQLite window
 * (`chiefd-core/src/store/event_journal.rs`) rather than a byte-capped file.
 * `BUS_EVENTS_MAX_BYTES` is consequently the SOLE authority for this bound.
 * `BusEventsBoundedAppend.test.ts` now pins that: the value, and that
 * neither producer inlines a literal instead of importing it.
 */
export const BUS_EVENTS_MAX_BYTES = 128 * 1024 * 1024;

const STAT_INTERVAL = 256;
const trackedSizes = new Map<string, { bytes: number; sinceStat: number }>();

/**
 * Append one JSONL line, rotating `path` to `<path>.1` first if this append
 * would exceed `maxBytes`. On-disk bytes for a stream therefore never
 * exceed `2 * maxBytes` plus one in-flight line -- identical bound shape to
 * `org-log.ts`'s `appendBoundedLine`, which this mirrors call-for-call.
 */
export function appendBoundedJsonlLine(path: string, line: string, maxBytes: number): void {
  const payload = `${line}\n`;
  const size = Buffer.byteLength(payload);
  let tracked = trackedSizes.get(path);
  const nearCap = tracked !== undefined && tracked.bytes + size > maxBytes / 2;
  if (!tracked || (nearCap && tracked.sinceStat >= STAT_INTERVAL) || tracked.bytes + size > maxBytes) {
    let onDisk = 0;
    try {
      onDisk = statSync(path).size;
    } catch {
      onDisk = 0;
    }
    tracked = { bytes: onDisk, sinceStat: 0 };
    trackedSizes.set(path, tracked);
  }
  if (tracked.bytes > 0 && tracked.bytes + size > maxBytes) {
    try {
      renameSync(path, `${path}.1`);
      tracked.bytes = 0;
    } catch {
      // A rotation that cannot be published must not stop the line from
      // being written; the next append re-stats and retries.
      tracked.sinceStat = STAT_INTERVAL;
    }
  }
  appendFileSync(path, payload, { encoding: "utf8", mode: 0o600 });
  tracked.bytes += size;
  tracked.sinceStat += 1;
}

/** Test seam: rotation tracking is process-local memoisation of file size. */
export function resetBusEventsSizeTrackingForTests(): void {
  trackedSizes.clear();
}
