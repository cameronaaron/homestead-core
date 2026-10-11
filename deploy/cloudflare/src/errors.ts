// Server errors the Worker has sent, kept in the Durable Object's storage.
// The core's own `homestead_http_requests_total` lives in memory and starts
// from zero each time the container wakes, and the container sleeps after
// 10 idle minutes, so a daily scrape almost always read 0 5xx. The Worker
// sees every response, including the ones it makes itself when the
// container won't start, and its storage outlives the container.

/** How far back `homestead_edge_server_errors_24h` looks. */
export const WINDOW_MS = 24 * 60 * 60 * 1000;
/** Most timestamps kept; a flood of errors can't grow storage without bound. */
export const MAX_KEPT = 10_000;

export interface ErrorLog {
  /** Every server error ever recorded, restarts included. */
  total: number;
  /** When each recent one happened (ms since the epoch), oldest first. */
  recent: number[];
}

export const EMPTY: ErrorLog = { total: 0, recent: [] };

/** The log with one more error at `now`, dropping what's out of the window. */
export function record(log: ErrorLog, now: number): ErrorLog {
  const recent = log.recent.filter((t) => t > now - WINDOW_MS);
  recent.push(now);
  return { total: log.total + 1, recent: recent.slice(-MAX_KEPT) };
}

/** Prometheus text for the log as of `now`, to append to the core's metrics. */
export function exposition(log: ErrorLog, now: number): string {
  const inWindow = log.recent.filter((t) => t > now - WINDOW_MS).length;
  return [
    "# HELP homestead_edge_server_errors_total 5xx responses (or thrown errors) the Worker has sent; survives container restarts.",
    "# TYPE homestead_edge_server_errors_total counter",
    `homestead_edge_server_errors_total ${log.total}`,
    "# HELP homestead_edge_server_errors_24h 5xx responses (or thrown errors) the Worker sent in the last 24 hours.",
    "# TYPE homestead_edge_server_errors_24h gauge",
    `homestead_edge_server_errors_24h ${inWindow}`,
    "",
  ].join("\n");
}
