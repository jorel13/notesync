import { requestUrl } from "obsidian";
import type { HttpTransport } from "../api/http";

/**
 * HTTP through Obsidian's `requestUrl`, which works the same on desktop and
 * mobile and isn't subject to browser CORS rules.
 */
export const obsidianTransport: HttpTransport = async (request) => {
  const body =
    request.body instanceof Uint8Array
      ? (request.body.buffer.slice(
          request.body.byteOffset,
          request.body.byteOffset + request.body.byteLength,
        ) as ArrayBuffer)
      : request.body;
  const response = await requestUrl({
    url: request.url,
    method: request.method,
    headers: request.headers,
    contentType: request.headers["content-type"],
    body,
    throw: false,
  });
  return { status: response.status, body: new Uint8Array(response.arrayBuffer) };
};
