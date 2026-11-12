import { requestUrl } from "obsidian";
import type { HttpTransport } from "../api/http";

/**
 * HTTP through Obsidian's `requestUrl`, which works the same on desktop and
 * mobile and isn't subject to browser CORS rules.
 */
export const obsidianTransport: HttpTransport = async (request) => {
  const body = request.body instanceof Uint8Array ? toArrayBuffer(request.body) : request.body;
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

/** The bytes as an ArrayBuffer, copying only if they're part of a larger buffer. */
export function toArrayBuffer(data: Uint8Array): ArrayBuffer {
  if (data.byteOffset === 0 && data.byteLength === data.buffer.byteLength) {
    return data.buffer as ArrayBuffer;
  }
  return data.buffer.slice(data.byteOffset, data.byteOffset + data.byteLength) as ArrayBuffer;
}
