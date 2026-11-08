import type { PathConflict } from "./types";

/** An error response from the server, in its `{"error": {"code", "message"}}` format. */
export class ServerError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
    readonly conflicts: PathConflict[] = [],
  ) {
    super(message);
    this.name = "ServerError";
  }
}

/** A sync would delete more files than the safety limit allows. */
export class MassDeleteError extends Error {
  constructor(
    readonly count: number,
    readonly where: "local" | "server",
  ) {
    super(
      where === "local"
        ? `Sync would move ${count} files on this device to the trash. Confirm to continue.`
        : `Sync would delete ${count} files from the server. Confirm to continue.`,
    );
    this.name = "MassDeleteError";
  }
}

/** Errors that a fresh attempt (pull, merge, push again) can resolve. */
export function isRetryable(err: unknown): boolean {
  return err instanceof ServerError && (err.code === "conflict" || err.code === "missing_blob");
}
