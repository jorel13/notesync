import { diff3Merge } from "node-diff3";

export type MergeOutcome = { clean: true; text: string } | { clean: false };

/**
 * Three-way merge of text by lines. Edits that don't overlap are combined;
 * identical edits on both sides are fine. Line endings follow the local copy.
 */
export function mergeText(base: string, local: string, remote: string): MergeOutcome {
  const eol = local.includes("\r\n") ? "\r\n" : "\n";
  const lines = (text: string) => text.split(/\r?\n/);
  const regions = diff3Merge(lines(local), lines(base), lines(remote), {
    excludeFalseConflicts: true,
  });
  const merged: string[] = [];
  for (const region of regions) {
    if (region.conflict) return { clean: false };
    if (region.ok) merged.push(...region.ok);
  }
  return { clean: true, text: merged.join(eol) };
}

const decoder = new TextDecoder("utf-8", { fatal: true });
const encoder = new TextEncoder();

/** Decodes UTF-8, or returns null for content that isn't valid text. */
export function decodeText(data: Uint8Array): string | null {
  try {
    return decoder.decode(data);
  } catch {
    return null;
  }
}

export function encodeText(text: string): Uint8Array {
  return encoder.encode(text);
}
