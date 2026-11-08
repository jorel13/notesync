// Two simulated devices syncing through the real server over HTTP.
//
// Needs the server built first (`cargo build` in ../server). Skipped when the
// binary is missing, unless NOTESYNC_E2E=1 (as in CI), which makes it fail instead.

import { spawn, spawnSync, type ChildProcess } from "node:child_process";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { createServer, type AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { afterAll, beforeAll, describe, expect, it } from "vitest";

import { HttpServerApi, login } from "../src/api/http";
import { ServerError } from "../src/engine/errors";
import type { ServerApi } from "../src/engine/types";
import { expectSameFiles, makeDevice, syncAll, type Device } from "./support/devices";

const BINARY = fileURLToPath(
  new URL(
    `../../server/target/debug/notesync-server${process.platform === "win32" ? ".exe" : ""}`,
    import.meta.url,
  ),
);
const PASSWORD = "end-to-end password";
const available = existsSync(BINARY);
if (!available && process.env.NOTESYNC_E2E === "1") {
  throw new Error(`Server binary not found at ${BINARY}; run cargo build in server/.`);
}

let server: ChildProcess;
let dataDir: string;
let url: string;
let vaultCounter = 0;

function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const probe = createServer();
    probe.on("error", reject);
    probe.listen(0, "127.0.0.1", () => {
      const { port } = probe.address() as AddressInfo;
      probe.close(() => resolve(port));
    });
  });
}

async function waitForHealth(): Promise<void> {
  for (let i = 0; i < 150; i++) {
    try {
      const res = await fetch(`${url}/api/v1/health`);
      if (res.ok) return;
    } catch {
      // Not listening yet.
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error("server did not start");
}

/** A fresh vault with a PC and a phone, each signed in separately. */
async function twoDevices(): Promise<[Device, Device, HttpServerApi]> {
  const pcLogin = await login(url, "alice", PASSWORD, "PC");
  const phoneLogin = await login(url, "alice", PASSWORD, "Phone");
  const pcApi = new HttpServerApi(url, pcLogin.token);
  const vault = await pcApi.createVault(`Vault ${++vaultCounter}`);
  return [
    makeDevice(pcApi, vault.id, "PC"),
    makeDevice(new HttpServerApi(url, phoneLogin.token), vault.id, "Phone"),
    pcApi,
  ];
}

describe.skipIf(!available)("end to end with the real server", () => {
  beforeAll(async () => {
    dataDir = mkdtempSync(join(tmpdir(), "notesync-e2e-"));
    const port = await freePort();
    url = `http://127.0.0.1:${port}`;
    const env = {
      ...process.env,
      NOTESYNC_DATA_DIR: dataDir,
      NOTESYNC_BIND_ADDR: `127.0.0.1:${port}`,
      NOTESYNC_LOG: "warn",
    };
    const add = spawnSync(BINARY, ["user", "add", "alice", "--password-stdin"], {
      env,
      input: `${PASSWORD}\n`,
      encoding: "utf8",
    });
    if (add.status !== 0) throw new Error(`creating the user failed: ${add.stderr}`);
    server = spawn(BINARY, ["serve"], { env, stdio: "ignore" });
    await waitForHealth();
  });

  afterAll(async () => {
    if (server && server.exitCode === null) {
      const exited = new Promise((r) => server.once("exit", r));
      server.kill();
      await exited;
    }
    if (dataDir) rmSync(dataDir, { recursive: true, force: true });
  });

  it("rejects wrong passwords and invalid tokens", async () => {
    await expect(login(url, "alice", "wrong password", "PC")).rejects.toMatchObject({
      status: 401,
      code: "unauthorized",
    });
    const api: ServerApi = new HttpServerApi(url, "ns_invalid");
    const error = await api.state(1).catch((e: unknown) => e);
    expect(error).toBeInstanceOf(ServerError);
    expect(error).toMatchObject({ status: 401 });
  });

  it("syncs creates, edits, renames and deletes in both directions", async () => {
    const [pc, phone] = await twoDevices();
    pc.fs.setText("Daily/2026-11-06.md", "# Today\n");
    pc.fs.setText("Draft.md", "draft");
    pc.fs.setBytes("Attachments/photo.bin", new Uint8Array(200_000).map((_, i) => (i * 31) % 256));
    await syncAll(pc, phone);
    expectSameFiles(pc, phone);

    phone.fs.setText("Daily/2026-11-06.md", "# Today\n\n- wrote on phone\n");
    phone.fs.move("Draft.md", "Final.md");
    pc.fs.remove("Attachments/photo.bin");
    await syncAll(pc, phone);

    expectSameFiles(pc, phone);
    expect(pc.fs.paths()).toEqual(["Daily/2026-11-06.md", "Final.md"]);
    expect(pc.fs.renames).toEqual([{ from: "Draft.md", to: "Final.md" }]);
  });

  it("merges separate edits and keeps conflict copies for overlapping ones", async () => {
    const [pc, phone] = await twoDevices();
    pc.fs.setText("Merge.md", "a\nb\nc\n");
    pc.fs.setText("Clash.md", "same line\n");
    await syncAll(pc, phone);

    pc.fs.setText("Merge.md", "a from pc\nb\nc\n");
    phone.fs.setText("Merge.md", "a\nb\nc from phone\n");
    pc.fs.setText("Clash.md", "pc wording\n");
    phone.fs.setText("Clash.md", "phone wording\n");
    await pc.engine.sync();
    const report = await phone.engine.sync();
    expect(report.merged).toEqual(["Merge.md"]);
    expect(report.conflictCopies).toEqual(["Clash (conflict 2026-11-06 1432 Phone).md"]);
    await syncAll(pc, phone);

    expectSameFiles(pc, phone);
    expect(pc.fs.text("Merge.md")).toBe("a from pc\nb\nc from phone\n");
    expect(pc.fs.text("Clash.md")).toBe("pc wording\n");
    expect(pc.fs.text("Clash (conflict 2026-11-06 1432 Phone).md")).toBe("phone wording\n");
  });

  it("converges after many offline edits on both devices", async () => {
    const [pc, phone] = await twoDevices();
    for (let i = 0; i < 20; i++) pc.fs.setText(`Shared/${i}.md`, `line A\nline B ${i}\nline C\n`);
    await syncAll(pc, phone);

    for (let i = 0; i < 20; i++) {
      if (i % 3 === 0) pc.fs.setText(`Shared/${i}.md`, `line A edited\nline B ${i}\nline C\n`);
      if (i % 4 === 0) phone.fs.setText(`Shared/${i}.md`, `line A\nline B ${i}\nline C edited\n`);
      if (i % 5 === 1) pc.fs.remove(`Shared/${i}.md`);
      if (i % 7 === 2) phone.fs.setText(`Phone only ${i}.md`, "new");
    }
    await syncAll(pc, phone);
    expectSameFiles(pc, phone);
    expect(pc.fs.text("Shared/0.md")).toBe("line A edited\nline B 0\nline C edited\n");
  });

  it("keeps vaults separate", async () => {
    const [pc] = await twoDevices();
    const [other] = await twoDevices();
    pc.fs.setText("Only here.md", "x");
    await pc.engine.sync();
    await other.engine.sync();
    expect(other.fs.paths()).toEqual([]);
  });

  it("recovers when a sync is cut off mid-way", async () => {
    const [pc, phone, pcApi] = await twoDevices();
    let calls = 0;
    const flaky: ServerApi = {
      changes: (...a) => pcApi.changes(...a),
      state: (...a) => pcApi.state(...a),
      missing: (...a) => pcApi.missing(...a),
      upload: async (...a) => {
        if (++calls === 2) throw new Error("connection reset");
        return pcApi.upload(...a);
      },
      download: (...a) => pcApi.download(...a),
      commit: (...a) => pcApi.commit(...a),
    };
    const vaultId = (await pcApi.listVaults()).at(-1)!.id;
    const interrupted = makeDevice(flaky, vaultId, "PC", {}, pc);
    for (let i = 0; i < 5; i++) pc.fs.setText(`File ${i}.md`, `content ${i}`);

    await expect(interrupted.engine.sync()).rejects.toThrow("connection reset");
    await interrupted.engine.sync();
    await phone.engine.sync();
    expectSameFiles(pc, phone);
    expect(phone.fs.paths()).toHaveLength(5);
  });
});
