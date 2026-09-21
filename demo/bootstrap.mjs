// Provisions everything the demo needs, idempotently.
//
// The story is the one Invokr is actually used for: a mandate is registered
// with a bank, the bank does not answer straight away, so a job asks Aarokya
// for its status every minute until the status is terminal — and then cancels
// itself. Aarokya is `invokr-mock-server`, which prints what it did.
//
// Act 1 of the demo builds the config, secret, payload spec and endpoint live,
// so this file leaves the mandates workspace empty of exactly those four — the
// first run of act 1 in a session should be four real creations. Everything
// else is upserted, so act 2 works even if you skip act 1 and a database
// provisioned by an older version of this file gets brought up to date instead
// of quietly keeping a stale spec.

const ORG = { name: "Juspay", slug: "juspay" };

// Two workspaces, same endpoint name in both. The two-teams take fires into
// each and shows the rows landing in different schemas.
export const WORKSPACES = [
  { key: "a", name: "Mandates", slug: "mandates" },
  { key: "b", name: "Rides", slug: "rides" },
];

// What act 1 builds, in the order it builds it. Exported so the page names the
// same things this file does.
export const SETUP = {
  config: "aarokya-config",
  secret: "aarokya-callback-auth",
  payloadSpec: "mandate-input",
  endpoint: "aarokya-mandate-registration-sync",
};

/// Long gaps on purpose: a bank that is not answering yet is not a bank you
/// hammer. This is the policy from the team's own endpoint.
const RETRY = {
  max_attempts: 3,
  backoff: "exponential",
  initial_delay_ms: 60000,
  max_delay_ms: 600000,
};

/// Short gaps, for the take that shows a retry inside a demo slot.
const RETRY_FAST = {
  max_attempts: 3,
  backoff: "exponential",
  initial_delay_ms: 2000,
  max_delay_ms: 30000,
};

const MANDATE_INPUT_SCHEMA = {
  type: "object",
  properties: {
    mandate_id: { type: "string" },
    // How the demo dials the fake bank: how many checks before it says ACTIVE,
    // and how many times it should fail outright first. Strings, because a
    // templated header that resolves to a number is dropped before it is sent.
    checks: { type: "string" },
    fail_times: { type: "string" },
  },
  required: ["mandate_id"],
};

/// The endpoint Act 1 builds, and the shape the team actually runs: a URL
/// assembled from config and input, an Authorization header resolved from the
/// secret store at execution time, and a retry policy measured in minutes.
export function mandateSyncSpec() {
  return {
    name: SETUP.endpoint,
    type: "HTTP",
    config: SETUP.config,
    payload_spec: SETUP.payloadSpec,
    spec: {
      url: "{{config.base_url}}/mandates/{{input.mandate_id}}/sync",
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Authorization: "{{secret.aarokya-callback-auth}}",
        "api-version": "2026-08-15",
        // Which team is asking, out of this workspace's own config.
        "x-team": "{{config.team}}",
        // Demo dials. The real endpoint has neither.
        "x-terminal-after": "{{input.checks}}",
        "x-fail-times": "{{input.fail_times}}",
      },
      timeout_ms: 30000,
      expected_status_codes: [200, 201],
    },
    retry_policy: RETRY,
  };
}

/// The endpoints the other takes need.
///
/// Deliberately self-contained: none of them references the config, secret or
/// payload spec that act 1 builds. Invokr refuses to delete a config a live
/// endpoint still points at — rightly — and act 1's whole point is deleting
/// those four and watching them come back, so nothing else may depend on them.
function supportingEndpoints(mockUrl) {
  return [
    {
      // The same call as the hero, with retries measured in seconds so the
      // retry take fits in a demo slot instead of a coffee break.
      name: "aarokya-mandate-sync-impatient",
      type: "HTTP",
      spec: {
        url: `${mockUrl}/mandates/{{input.mandate_id}}/sync`,
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          "api-version": "2026-08-15",
          "x-terminal-after": "{{input.checks}}",
          "x-fail-times": "{{input.fail_times}}",
        },
        timeout_ms: 30000,
        expected_status_codes: [200, 201],
      },
      retry_policy: RETRY_FAST,
    },
    {
      // Kafka and Redis Stream dispatches do not propagate the idempotency key
      // automatically the way HTTP does — it is templated in here by hand.
      name: "mandate-events-kafka",
      type: "KAFKA",
      spec: {
        bootstrap_servers: "localhost:9092",
        topic: "invokr-demo-mandates",
        key_template: "{{input.mandate_id}}",
        value_template: { mandate_id: "{{input.mandate_id}}", execution_id: "{{execution.execution_id}}" },
        headers: { "idempotency-key": "{{execution.idempotency_key}}" },
        acks: "all",
        timeout_ms: 10000,
      },
      retry_policy: RETRY_FAST,
    },
    {
      name: "mandate-events-redis",
      type: "REDIS_STREAM",
      spec: {
        redis_url: "redis://127.0.0.1:6379",
        stream: "invokr-demo-mandates",
        fields_template: {
          mandate_id: "{{input.mandate_id}}",
          idempotency_key: "{{execution.idempotency_key}}",
        },
        max_len: 1000,
        approximate_trimming: true,
      },
      retry_policy: RETRY_FAST,
    },
  ];
}

export class InvokrAdmin {
  constructor({ baseUrl, apiKey }) {
    this.baseUrl = baseUrl.replace(/\/$/, "");
    this.apiKey = apiKey;
  }

  async call(method, path, { body, org, workspace } = {}) {
    const headers = { Authorization: `Bearer ${this.apiKey}` };
    if (body !== undefined) headers["Content-Type"] = "application/json";
    if (org) headers["X-Org-Id"] = org;
    if (workspace) headers["X-Workspace-Id"] = workspace;

    const res = await fetch(`${this.baseUrl}${path}`, {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    const text = await res.text();
    let parsed = null;
    try {
      parsed = text ? JSON.parse(text) : null;
    } catch {
      parsed = { raw: text };
    }
    return { status: res.status, ok: res.ok, body: parsed };
  }

  /// Create it, or update what is already there. Anything else throws loudly —
  /// a half-provisioned demo is worse than one that refuses to start.
  ///
  /// Create-then-update rather than delete-then-create, because an endpoint any
  /// job has ever pointed at cannot be deleted at all: `jobs.endpoint` is a
  /// foreign key, and a retired job still holds it.
  async upsert(label, collection, name, body) {
    const created = await this.call("POST", `/v1/${collection}`, { ...this.scope, body });
    if (created.ok) return created.body?.data ?? created.body;

    if (created.status === 409) {
      const { name: _name, ...rest } = body;
      const updated = await this.call("PUT", `/v1/${collection}/${name}`, { ...this.scope, body: rest });
      if (updated.ok) return updated.body?.data ?? updated.body;
      throw new Error(`${label}: update failed ${updated.status} ${JSON.stringify(updated.body)?.slice(0, 200)}`);
    }

    throw new Error(`${label}: ${created.status} ${JSON.stringify(created.body)?.slice(0, 250)}`);
  }
}

/// Does this build know about long-running jobs?
///
/// Behavioural, because nothing advertises it: a build with the feature
/// validates the endpoint's `async` block and rejects one that enables neither
/// polling nor callbacks. A build without it stores the block as opaque JSON
/// and happily says 201 — so a 201 here means "no support", and the throwaway
/// endpoint is deleted again.
async function probeLongRunning(api) {
  const name = "_demo_async_probe";
  const body = {
    name,
    type: "HTTP",
    spec: {
      url: "http://127.0.0.1:1/never",
      method: "POST",
      expected_status_codes: [200],
      async: { status_codes: [202] }, // neither poll nor callback: invalid there, ignored here
    },
  };

  await api.call("DELETE", `/v1/endpoints/${name}`, api.scope); // in case a probe leaked
  const res = await api.call("POST", "/v1/endpoints", { ...api.scope, body });
  if (res.ok) {
    await api.call("DELETE", `/v1/endpoints/${name}`, api.scope);
    return false;
  }
  return res.status === 400 || res.status === 422;
}

export async function bootstrap({ baseUrl, apiKey, mockUrl }) {
  const api = new InvokrAdmin({ baseUrl, apiKey });

  const orgs = await api.call("GET", "/v1/orgs");
  if (!orgs.ok) throw new Error(`cannot reach Invokr at ${baseUrl}: ${orgs.status}`);

  let org = (orgs.body?.data ?? []).find((o) => o.slug === ORG.slug);
  if (!org) {
    const created = await api.call("POST", "/v1/orgs", { body: ORG });
    if (!created.ok) throw new Error(`create org: ${created.status}`);
    org = created.body.data;
  }

  const result = {
    org_id: org.org_id,
    org_name: ORG.name,
    workspaces: {},
    endpoints: [],
    longRunning: false,
    // The page draws the setup act from this, so there is one definition of
    // the endpoint rather than one here and a copy in the browser.
    mandateSpec: mandateSyncSpec(),
    setup: SETUP,
  };

  const existingWs = await api.call("GET", `/v1/orgs/${org.org_id}/workspaces`);
  for (const ws of WORKSPACES) {
    let row = (existingWs.body?.data ?? []).find((w) => w.slug === ws.slug);
    if (!row) {
      const created = await api.call("POST", `/v1/orgs/${org.org_id}/workspaces`, {
        body: { name: ws.name, slug: ws.slug },
      });
      if (!created.ok) throw new Error(`create workspace ${ws.slug}: ${created.status}`);
      row = created.body.data;
    }
    result.workspaces[ws.key] = {
      key: ws.key,
      name: ws.name,
      slug: ws.slug,
      workspace_id: row.workspace_id,
      schema_name: row.schema_name,
    };

    api.scope = { org: org.org_id, workspace: row.workspace_id };

    // Workspace `a` is the one act 1 builds into, and it is left empty of those
    // four on purpose: the first run of act 1 in a session should be four real
    // creations, not four updates. The page re-creates them itself if someone
    // opens act 2 first.
    if (ws.key !== "a") {
      await api.upsert(`payload-spec in ${ws.slug}`, "payload-specs", SETUP.payloadSpec, {
        name: SETUP.payloadSpec,
        schema: MANDATE_INPUT_SCHEMA,
      });
      await api.upsert(`config in ${ws.slug}`, "configs", SETUP.config, {
        name: SETUP.config,
        values: { base_url: mockUrl, team: ws.slug },
      });
      // Not a real key — a fixture, so that what the demo resolves and masks is
      // recognisably fake if it ever ends up on a projector.
      await api.upsert(`secret in ${ws.slug}`, "secrets", SETUP.secret, {
        name: SETUP.secret,
        value: `Bearer aarokya-demo-${ws.slug}-7d41c9`,
      });
      await api.upsert(`endpoint ${SETUP.endpoint} in ${ws.slug}`, "endpoints", SETUP.endpoint, mandateSyncSpec());
    }

    for (const ep of supportingEndpoints(mockUrl)) {
      await api.upsert(`endpoint ${ep.name} in ${ws.slug}`, "endpoints", ep.name, ep);
      if (ws.key === "a") result.endpoints.push({ name: ep.name, type: ep.type });
    }
  }

  try {
    result.longRunning = await probeLongRunning(api);
  } catch {
    result.longRunning = false;
  }

  return result;
}
