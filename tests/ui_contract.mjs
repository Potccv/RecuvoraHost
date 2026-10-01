// Explicit integration check against a separately built UI and Host executable.
// node tests/ui_contract.mjs <host-executable> <ui-dist> <external-test-root>
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import { once } from "node:events";
import { mkdtemp, readFile, realpath, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { setTimeout as delay } from "node:timers/promises";

const [executableArg, uiArg, temporaryArg] = process.argv.slice(2);
assert(executableArg && uiArg && temporaryArg, "Supply Host executable, UI dist and external test root");
const executable = await realpath(executableArg);
const uiDir = await realpath(uiArg);
const temporary = await realpath(temporaryArg);
const source = await realpath(new URL("..", import.meta.url));
const relativeTemporary = path.relative(source, temporary);
assert(relativeTemporary === ".." || relativeTemporary.startsWith(`..${path.sep}`) || path.isAbsolute(relativeTemporary),
  "Test directory must be outside Host source");
const directory = await mkdtemp(path.join(temporary, "host-ui-contract-"));
const configPath = path.join(directory, "console.json");
const tokenPath = path.join(directory, "token");
const token = randomBytes(32).toString("hex");
const apiSource = await readFile(path.join(uiDir, "api.js"), "utf8");
const { ApiClient } = await import(`data:text/javascript;base64,${Buffer.from(apiSource).toString("base64")}`);
let child;

async function stop() {
  if (!child) return;
  const running = child;
  child = undefined;
  if (running.exitCode !== null || running.signalCode !== null) return;
  const exited = once(running, "exit");
  running.kill(); // Deliberate abrupt exit for persistent-receipt recovery checks.
  await Promise.race([exited, delay(10000, undefined, { ref: false }).then(() => {
    throw new Error("Host did not stop within 10 seconds");
  })]);
}

async function start() {
  let output = "";
  child = spawn(executable, ["serve", "--config", configPath], {
    windowsHide: true, stdio: ["ignore", "pipe", "pipe"],
  });
  child.stdout.on("data", bytes => { output = (output + bytes).slice(-16384); });
  child.stderr.on("data", bytes => { output = (output + bytes).slice(-16384); });
  let failure;
  child.on("error", error => { failure = error; });
  const deadline = Date.now() + 15000;
  while (Date.now() < deadline) {
    if (failure) throw failure;
    const address = output.match(/listening on (http:\/\/127\.0\.0\.1:\d+)/)?.[1];
    if (address) return address;
    assert.equal(child.exitCode, null, `Host exited during startup: ${output}`);
    await delay(25);
  }
  throw new Error(`Host startup timed out: ${output}`);
}

async function terminal(client, id) {
  const deadline = Date.now() + 10000;
  while (Date.now() < deadline) {
    const result = await client.operation(id);
    if (result.status !== "running") return result;
    await delay(20);
  }
  throw new Error("Operation did not finish");
}

try {
  await writeFile(tokenPath, token);
  await writeFile(configPath, JSON.stringify({
    schema_version: 1, listen: "127.0.0.1:0", token_file: tokenPath,
    operator: "integration-operator", data_dir: directory, ui_dir: uiDir,
    permissions: ["simulation.run", "logs.read", "operation.cancel"],
    allowed_origins: [],
  }));
  let address = await start();
  const client = new ApiClient();
  client.connect(address, token);
  assert.equal((await client.bootstrap()).mode, "live");
  const anonymous = await fetch(`${address}/api/v1/bootstrap`);
  assert.equal(anonymous.status, 401);
  const forbiddenOrigin = await fetch(`${address}/api/v1/bootstrap`, {
    headers: { Authorization: `Bearer ${token}`, Origin: "https://invalid.example" },
  });
  assert.equal(forbiddenOrigin.status, 403);
  for (const file of ["index.html", "api.js", "app.js", "styles.css"]) {
    const served = await fetch(`${address}/${file === "index.html" ? "" : file}`);
    assert.equal(served.status, 200);
    assert.equal(served.headers.get("x-content-type-options"), "nosniff");
    assert.deepEqual(Buffer.from(await served.arrayBuffer()), await readFile(path.join(uiDir, file)));
  }
  assert.equal((await fetch(`${address}/console.json`)).status, 404);
  const input = { operation_id: "ui-operation", task_id: "ui-task", target: "closed-simulation", scenario: "succeed", timeout_ms: 1000 };
  const accepted = await client.submit("/api/v1/simulations", input);
  assert.equal(accepted.operation_id, input.operation_id);
  const completed = await terminal(client, input.operation_id);
  assert.equal(completed.status, "completed");
  assert.equal(completed.result.task.state, "Succeeded");
  await assert.rejects(client.submit("/api/v1/simulations", input), error => error.status === 409 && !error.autoRetry);
  assert.equal((await client.list("simulation_tasks")).items[0].state, "succeeded");
  assert((await client.logs({ limit: 10 })).items.length > 0);
  await assert.rejects(client.submit("/api/v1/approvals/missing/approve", { revision: 1, reason: "test" }), error => error.status === 403);
  await client.submit("/api/v1/simulations", { ...input, operation_id: "interrupted", task_id: "interrupted-task", target: "other-target", scenario: "hang", timeout_ms: 60000 });
  await stop();
  address = await start();
  client.connect(address, token);
  assert.equal((await client.operation(input.operation_id)).status, "completed");
  assert.equal((await client.operation("interrupted")).status, "unknown");
  await assert.rejects(client.submit("/api/v1/simulations", { ...input, operation_id: "interrupted" }), error => error.status === 409);
  client.disconnect();
  console.log("UI client + Host + Core: authentication, static delivery, simulation, duplicate rejection and restart recovery passed.");
} finally {
  await stop();
  const resolved = await realpath(directory);
  assert.equal(path.dirname(resolved), temporary);
  assert(path.basename(resolved).startsWith("host-ui-contract-"));
  await rm(resolved, { recursive: true });
}
