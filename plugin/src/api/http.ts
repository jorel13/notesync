import manifest from "../../manifest.json";
import { ServerError } from "../engine/errors";
import type {
  ChangesPage,
  CommitChange,
  CommitResult,
  RemoteState,
  ServerApi,
  UploadProgress,
} from "../engine/types";

/** Sent with every request, so the server can refuse versions it no longer supports. */
export const CLIENT_VERSION: string = manifest.version;

export interface HttpRequest {
  method: string;
  url: string;
  headers: Record<string, string>;
  body?: Uint8Array | string;
}

export interface HttpResponse {
  status: number;
  body: Uint8Array;
}

/**
 * Sends one HTTP request. Must return error statuses as responses rather than
 * throwing. The plugin uses Obsidian's `requestUrl`; tests use `fetch`.
 */
export type HttpTransport = (request: HttpRequest) => Promise<HttpResponse>;

export const fetchTransport: HttpTransport = async (request) => {
  const response = await fetch(request.url, {
    method: request.method,
    headers: request.headers,
    body: request.body as BodyInit | undefined,
  });
  return { status: response.status, body: new Uint8Array(await response.arrayBuffer()) };
};

export interface VaultInfo {
  id: number;
  name: string;
  head_seq: number;
  created_at: number;
}

export interface UserInfo {
  id: number;
  username: string;
  is_admin: boolean;
}

/** A device token and when to replace it (Unix seconds). */
export interface IssuedToken {
  token: string;
  expires_at: number;
  refresh_after: number;
}

export interface LoginResult extends IssuedToken {
  device_id: number;
  user: UserInfo;
}

const decoder = new TextDecoder();

const LOCAL_HOSTS = new Set(["localhost", "127.0.0.1", "[::1]"]);

/**
 * Why a server address isn't safe to use, or null. Passwords and tokens must
 * travel over HTTPS; plain HTTP is only allowed to this computer, for testing.
 */
export function serverUrlProblem(serverUrl: string): string | null {
  let url: URL;
  try {
    url = new URL(serverUrl.trim());
  } catch {
    return "The server address isn't a valid URL, e.g. https://notes.example.ts.net";
  }
  if (url.protocol === "https:") return null;
  if (url.protocol === "http:" && (LOCAL_HOSTS.has(url.hostname) || url.hostname.endsWith(".localhost"))) {
    return null;
  }
  return "The server address must start with https:// (http:// is only allowed for localhost).";
}

/** `https://notes.example.ts.net/` -> `https://notes.example.ts.net/api/v1` */
export function apiBase(serverUrl: string): string {
  const problem = serverUrlProblem(serverUrl);
  if (problem) throw new ServerError(0, "insecure_url", problem);
  return `${serverUrl.trim().replace(/\/+$/, "")}/api/v1`;
}

async function send(
  transport: HttpTransport,
  method: string,
  url: string,
  token: string | null,
  body?: unknown,
): Promise<Uint8Array> {
  return (await sendFull(transport, method, url, token, body)).body;
}

/** Like `send`, but returns the status too. */
async function sendFull(
  transport: HttpTransport,
  method: string,
  url: string,
  token: string | null,
  body?: unknown,
  extraHeaders: Record<string, string> = {},
): Promise<HttpResponse> {
  const headers: Record<string, string> = { "x-notesync-client": CLIENT_VERSION, ...extraHeaders };
  if (token) headers.authorization = `Bearer ${token}`;
  let payload: Uint8Array | string | undefined;
  if (body instanceof Uint8Array) {
    headers["content-type"] = "application/octet-stream";
    payload = body;
  } else if (body !== undefined) {
    headers["content-type"] = "application/json";
    payload = JSON.stringify(body);
  }
  const response = await transport({ method, url, headers, body: payload });
  if (response.status >= 200 && response.status < 300) return response;
  throw toServerError(response);
}

function toServerError(response: HttpResponse): ServerError {
  try {
    const error = JSON.parse(decoder.decode(response.body))?.error;
    if (error && typeof error.code === "string") {
      return new ServerError(
        response.status,
        error.code,
        typeof error.message === "string" ? error.message : error.code,
        Array.isArray(error.conflicts) ? error.conflicts : [],
      );
    }
  } catch {
    // Not a JSON error body; fall through.
  }
  return new ServerError(
    response.status,
    `http_${response.status}`,
    `The server returned HTTP ${response.status}.`,
  );
}

function parseJson<T>(bytes: Uint8Array): T {
  return JSON.parse(decoder.decode(bytes)) as T;
}

export async function login(
  serverUrl: string,
  username: string,
  password: string,
  deviceName: string,
  transport: HttpTransport = fetchTransport,
): Promise<LoginResult> {
  const bytes = await send(transport, "POST", `${apiBase(serverUrl)}/auth/login`, null, {
    username,
    password,
    device_name: deviceName,
  });
  return parseJson<LoginResult>(bytes);
}

export class HttpServerApi implements ServerApi {
  private readonly base: string;

  constructor(
    serverUrl: string,
    private readonly token: string,
    private readonly transport: HttpTransport = fetchTransport,
  ) {
    this.base = apiBase(serverUrl);
  }

  private raw(method: string, path: string, body?: unknown): Promise<Uint8Array> {
    return send(this.transport, method, `${this.base}${path}`, this.token, body);
  }

  private async json<T>(method: string, path: string, body?: unknown): Promise<T> {
    return parseJson<T>(await this.raw(method, path, body));
  }

  // Account and vault management.

  me(): Promise<UserInfo> {
    return this.json("GET", "/me");
  }

  async logout(): Promise<void> {
    await this.raw("POST", "/auth/logout");
  }

  /** Replaces this device's token; the current one stops working shortly after. */
  refresh(): Promise<IssuedToken> {
    return this.json("POST", "/auth/refresh");
  }

  listVaults(): Promise<VaultInfo[]> {
    return this.json("GET", "/vaults");
  }

  createVault(name: string): Promise<VaultInfo> {
    return this.json("POST", "/vaults", { name });
  }

  // ServerApi, used by the sync engine.

  changes(vaultId: number, since: number, limit = 500): Promise<ChangesPage> {
    return this.json("GET", `/vaults/${vaultId}/changes?since=${since}&limit=${limit}`);
  }

  state(vaultId: number): Promise<RemoteState> {
    return this.json("GET", `/vaults/${vaultId}/state`);
  }

  async missing(vaultId: number, hashes: string[]): Promise<string[]> {
    const result = await this.json<{ missing: string[] }>(
      "POST",
      `/vaults/${vaultId}/blobs/missing`,
      { hashes },
    );
    return result.missing;
  }

  async upload(vaultId: number, hash: string, data: Uint8Array): Promise<void> {
    await this.raw("PUT", `/vaults/${vaultId}/blobs/${hash}`, data);
  }

  download(vaultId: number, hash: string): Promise<Uint8Array> {
    return this.raw("GET", `/vaults/${vaultId}/blobs/${hash}`);
  }

  async downloadRange(
    vaultId: number,
    hash: string,
    offset: number,
    length: number,
  ): Promise<{ data: Uint8Array; whole: boolean }> {
    const response = await sendFull(
      this.transport,
      "GET",
      `${this.base}/vaults/${vaultId}/blobs/${hash}`,
      this.token,
      undefined,
      { range: `bytes=${offset}-${offset + length - 1}` },
    );
    // 206 is the requested piece; 200 means the server sent everything.
    return { data: response.body, whole: response.status !== 206 };
  }

  async uploadProgress(vaultId: number, hash: string): Promise<UploadProgress | null> {
    try {
      return await this.json<UploadProgress>("GET", `/vaults/${vaultId}/blobs/${hash}/parts`);
    } catch (err) {
      // A server without this endpoint answers with a bare 404, not a JSON error.
      if (err instanceof ServerError && err.code === "http_404") return null;
      throw err;
    }
  }

  uploadPart(
    vaultId: number,
    hash: string,
    offset: number,
    total: number,
    data: Uint8Array,
  ): Promise<UploadProgress> {
    return this.json(
      "PUT",
      `/vaults/${vaultId}/blobs/${hash}/parts?offset=${offset}&total=${total}`,
      data,
    );
  }

  commit(vaultId: number, changes: CommitChange[]): Promise<CommitResult> {
    return this.json("POST", `/vaults/${vaultId}/commit`, { changes });
  }
}
