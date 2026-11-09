// Path rules shared with the server (server/src/paths.rs), checked locally so
// unsyncable files are skipped with a clear reason instead of failing a commit.

export const MAX_PATH_BYTES = 1024;

/** Folders that are never synced. */
export const ALWAYS_EXCLUDED = [".obsidian/", ".trash/", ".git/"];

const FORBIDDEN_CHARS = new Set(['<', '>', ':', '"', '|', '?', '*', '\\']);

/** Characters Windows can't store in a file name: the set above plus control characters. */
function isForbiddenChar(char: string): boolean {
  const code = char.charCodeAt(0);
  return code < 0x20 || code === 0x7f || FORBIDDEN_CHARS.has(char);
}

const RESERVED = new Set([
  "con", "prn", "aux", "nul",
  "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9",
  "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
]);

/** Why the server would reject this path, or null if it's fine. */
export function pathProblem(path: string): string | null {
  if (path.length === 0) return "path is empty";
  if (path !== path.normalize("NFC")) return "path is not in Unicode NFC form";
  if (new TextEncoder().encode(path).length > MAX_PATH_BYTES) return "path is too long";
  if (path.startsWith("/")) return "path must be relative";
  for (const segment of path.split("/")) {
    if (segment.length === 0) return "path has an empty segment";
    if (segment === "." || segment === "..") return "path contains '.' or '..'";
    if ([...segment].some(isForbiddenChar)) return "name contains a character Windows doesn't allow";
    if (segment.endsWith(" ") || segment.endsWith(".")) return "name ends with a space or dot";
    const stem = segment.split(".")[0].toLowerCase();
    if (RESERVED.has(stem)) return "name is reserved on Windows";
  }
  return null;
}

/** Key used to detect paths that differ only by letter case (matches the server). */
export function pathKey(path: string): string {
  return path.toLowerCase();
}

/**
 * True if `path` is inside one of `prefixes` (folders end with "/") or equals
 * one. Ignores letter case, because Windows and Android do: `.Obsidian/` is
 * the same folder as `.obsidian/` there.
 */
export function isExcluded(path: string, prefixes: string[]): boolean {
  const p = pathKey(path);
  return prefixes.some((raw) => {
    const prefix = pathKey(raw);
    return prefix.endsWith("/") ? p.startsWith(prefix) || p === prefix.slice(0, -1) : p === prefix;
  });
}

export function extension(path: string): string {
  const name = path.slice(path.lastIndexOf("/") + 1);
  const dot = name.lastIndexOf(".");
  return dot > 0 ? name.slice(dot + 1).toLowerCase() : "";
}

/**
 * `Folder/Note.md` -> `Folder/Note (conflict 2026-11-06 1432 Pixel).md`, adding
 * " 2", " 3"... if that name is taken.
 */
export function conflictCopyPath(
  path: string,
  device: string,
  when: Date,
  taken: (candidate: string) => boolean,
): string {
  const slash = path.lastIndexOf("/");
  const dir = path.slice(0, slash + 1);
  const name = path.slice(slash + 1);
  const dot = name.lastIndexOf(".");
  const stem = dot > 0 ? name.slice(0, dot) : name;
  const ext = dot > 0 ? name.slice(dot) : "";
  const pad = (n: number) => String(n).padStart(2, "0");
  const stamp = `${when.getFullYear()}-${pad(when.getMonth() + 1)}-${pad(when.getDate())} ${pad(when.getHours())}${pad(when.getMinutes())}`;
  const safeDevice =
    [...device].map((c) => (c === "/" || isForbiddenChar(c) ? "-" : c)).join("").trim() || "device";
  for (let n = 1; ; n++) {
    const suffix = n === 1 ? "" : ` ${n}`;
    const candidate = `${dir}${stem} (conflict ${stamp} ${safeDevice}${suffix})${ext}`;
    if (!taken(candidate)) return candidate;
  }
}
