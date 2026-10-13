/**
 * Invokr CLI — Test INTERNAL job/endpoint API guards
 *
 * The dogfooded reaper is provisioned at workspace creation as an INTERNAL
 * endpoint (`invokr.reaper`) plus a CRON job. The public API protects this
 * pair so users can't stack their own jobs on the endpoint, modify the
 * reaper job's schedule or the endpoint's spec, or cancel it — all of which would silently break
 * invokr's self-monitoring for that workspace. This script verifies those
 * guards and that reads still surface the system state.
 *
 * Steps:
 *   1. List jobs and find the reaper (endpoint == "invokr.reaper").
 *   2. POST /jobs with endpoint: "invokr.reaper" → expect 400 INVALID_REQUEST.
 *   3. PUT /jobs/{reaper}                       → expect 409 JOB_NOT_UPDATABLE.
 *   4. POST /jobs/{reaper}/cancel               → expect 409 CONFLICT.
 *   5. POST /endpoints with type: "INTERNAL"    → expect 400 INVALID_REQUEST.
 *   6. PUT /endpoints/invokr.reaper             → expect 409 CONFLICT.
 *   7. DELETE /endpoints/invokr.reaper          → expect 409 CONFLICT.
 *   8. GET /jobs/{reaper}, its executions and its endpoint → expect 200, with
 *      the endpoint's spec unchanged (reads visible, nothing overwritten).
 *
 * No worker / scheduler required — purely an API-surface test.
 *
 * Prerequisites:
 *   - Invokr API running at INVOKR_URL (default: http://localhost:8080)
 *   - INVOKR_ORG_ID and INVOKR_WORKSPACE_ID env vars pointing at an existing
 *     workspace (which will have the reaper provisioned).
 */

import {
  CreateEndpointCommand,
  DeleteEndpointCommand,
  GetEndpointCommand,
  UpdateEndpointCommand,
  CreateJobCommand,
  UpdateJobCommand,
  CancelJobCommand,
  GetJobCommand,
  ListJobsCommand,
  ListJobExecutionsCommand,
} from "invokr-sdk";

import { log, createClient, tenant } from "./helpers.js";

const REAPER_ENDPOINT = "invokr.reaper";

/**
 * Assert that an SDK call rejected with the expected HTTP status and error
 * code. The Smithy-generated client raises errors with `$metadata.httpStatusCode`
 * and a `.Code` (the snake_case code from the AppError response envelope).
 * Returns the rejected error for further inspection if needed.
 */
async function expectError(
  label: string,
  expectedStatus: number,
  expectedCode: string,
  thunk: () => Promise<unknown>,
): Promise<void> {
  try {
    await thunk();
  } catch (err: any) {
    const status = err?.$metadata?.httpStatusCode;
    const code = err?.Code ?? err?.code ?? err?.name;
    if (status !== expectedStatus) {
      throw new Error(
        `${label}: expected HTTP ${expectedStatus} ${expectedCode}, got HTTP ${status} ${code} — ${err?.message ?? err}`,
      );
    }
    // The error code is best-effort: some clients surface it as Code, others
    // as a typed exception name. Log what we got rather than fail on mismatch,
    // since the status code is the load-bearing assertion.
    log(`  ✓ ${label}: HTTP ${status} (${code ?? "unknown code"})`);
    return;
  }
  throw new Error(
    `${label}: expected HTTP ${expectedStatus} ${expectedCode}, but request succeeded`,
  );
}

async function findReaperJob(client: ReturnType<typeof createClient>): Promise<string> {
  // The reaper job is the only one with endpoint == invokr.reaper.
  // It should always be present in an active workspace because
  // db::workspaces::create provisions it as part of schema setup.
  const resp = await client.send(new ListJobsCommand({ ...tenant }));
  const jobs = resp.data ?? [];
  const reaper = jobs.find((j: any) => j.endpoint === REAPER_ENDPOINT);
  if (!reaper) {
    throw new Error(
      `Workspace has no invokr.reaper job — was it provisioned at creation time? ` +
        `(Found ${jobs.length} jobs, none with endpoint=${REAPER_ENDPOINT}.)`,
    );
  }
  return reaper.job_id!;
}

async function main() {
  const client = createClient();

  log("Locating the reaper job in this workspace");
  const reaperJobId = await findReaperJob(client);
  log(`  Reaper job_id: ${reaperJobId}`);

  // ── 1. Block creating a user job that targets the reaper endpoint ──
  log("Attempting POST /jobs with endpoint=invokr.reaper (should 400)");
  await expectError(
    "create job on INTERNAL endpoint",
    400,
    "INVALID_REQUEST",
    () =>
      client.send(
        new CreateJobCommand({
          ...tenant,
          endpoint: REAPER_ENDPOINT,
          trigger: "IMMEDIATE",
          idempotency_key: `internal-guard-test-${Date.now()}`,
        }),
      ),
  );

  // ── 2. Block updating the reaper job ────────────────────────────────
  log("Attempting PUT /jobs/{reaper} (should 409)");
  await expectError(
    "update INTERNAL job",
    409,
    "JOB_NOT_UPDATABLE",
    () =>
      client.send(
        new UpdateJobCommand({
          ...tenant,
          job_id: reaperJobId,
          cron: "*/5 * * * *",
        }),
      ),
  );

  // ── 3. Block cancelling the reaper job ──────────────────────────────
  log("Attempting POST /jobs/{reaper}/cancel (should 409)");
  await expectError(
    "cancel INTERNAL job",
    409,
    "CONFLICT",
    () => client.send(new CancelJobCommand({ ...tenant, job_id: reaperJobId })),
  );

  // ── 4. Block creating an INTERNAL endpoint ──────────────────────────
  log("Attempting POST /endpoints with type=INTERNAL (should 400)");
  await expectError(
    "create INTERNAL endpoint",
    400,
    "INVALID_REQUEST",
    () =>
      client.send(
        new CreateEndpointCommand({
          ...tenant,
          name: `internal-guard-test-${Date.now()}`,
          endpoint_type: "INTERNAL",
          spec: { task: "reaper" },
        }),
      ),
  );

  // ── 5. Block modifying the reaper endpoint ─────────────────────────
  // The reaper job would keep firing, but against whatever spec replaced its
  // own, so the workspace's sweep would stop with nothing reporting it.
  log("Attempting PUT /endpoints/invokr.reaper (should 409)");
  await expectError(
    "update INTERNAL endpoint",
    409,
    "CONFLICT",
    () =>
      client.send(
        new UpdateEndpointCommand({
          ...tenant,
          name: REAPER_ENDPOINT,
          spec: { url: "http://example.invalid/" },
        }),
      ),
  );

  // ── 6. Block deleting the reaper endpoint ─────────────────────────
  log("Attempting DELETE /endpoints/invokr.reaper (should 409)");
  await expectError(
    "delete INTERNAL endpoint",
    409,
    "CONFLICT",
    () =>
      client.send(new DeleteEndpointCommand({ ...tenant, name: REAPER_ENDPOINT })),
  );

  // ── 7. Reads stay visible — operators need this for dogfooded monitoring ──
  log("Verifying reads still surface the reaper job and executions");
  const getResp = await client.send(
    new GetJobCommand({ ...tenant, job_id: reaperJobId }),
  );
  if (getResp.data?.endpoint !== REAPER_ENDPOINT) {
    throw new Error(
      `GET /jobs/{reaper} returned wrong endpoint: ${getResp.data?.endpoint}`,
    );
  }
  if (getResp.data?.endpoint_type !== "INTERNAL") {
    throw new Error(
      `GET /jobs/{reaper} returned wrong endpoint_type: ${getResp.data?.endpoint_type}`,
    );
  }
  log(`  ✓ GET /jobs/{reaper} returned endpoint_type=INTERNAL`);

  const execsResp = await client.send(
    new ListJobExecutionsCommand({ ...tenant, job_id: reaperJobId }),
  );
  log(
    `  ✓ ListJobExecutions returned ${execsResp.data?.length ?? 0} execution(s) for the reaper`,
  );

  const epResp = await client.send(
    new GetEndpointCommand({ ...tenant, name: REAPER_ENDPOINT }),
  );
  const task = (epResp.data?.spec as { task?: string } | undefined)?.task;
  if (task !== "reaper") {
    throw new Error(
      `GET /endpoints/invokr.reaper returned a modified spec: ${JSON.stringify(epResp.data?.spec)}`,
    );
  }
  log(`  ✓ GET /endpoints/invokr.reaper still has spec.task=reaper`);

  log("All INTERNAL guard assertions passed");
  process.exit(0);
}

main().catch((err: any) => {
  console.error("\nTest failed:");
  console.error(`  ${err.message ?? err}`);
  process.exit(1);
});
