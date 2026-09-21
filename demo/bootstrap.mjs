// Provisions everything the demo scenes need, idempotently.
//
// Every call is "create, or accept that it already exists", so the demo can be
// re-bootstrapped between rehearsals without a database reset. The objects are
// deliberately boring — the point of the demo is the execution path, not the
// fixtures.

const ORG = { name: "Invokr Demo", slug: "invokr-demo" };

// Two workspaces, same endpoint name in both. Scene 7 fires into each and shows
// the rows landing in different schemas.
export const WORKSPACES = [
  { key: "a", name: "Payments", slug: "payments" },
  { key: "b", name: "Risk", slug: "risk" },
];

const RETRY_FAST = {
  max_attempts: 3,
  backoff: "exponential",
  initial_delay_ms: 2000,
  max_delay_ms: 30000,
};

function endpoints(mockUrl) {
  return [
    {
      name: "send-welcome-email",
      type: "HTTP",
      payload_spec: "order-input",
      config: "email-service",
      spec: {
        url: "{{config.api_base_url}}/echo",
        method: "POST",
        headers: {
          // Resolved from the encrypted secret store at execution time. The demo
          // shows this rendered as bullets to make the point that the plaintext
          // never leaves the worker.
          Authorization: "Bearer {{secret.email_api_key}}",
          "Content-Type": "application/json",
        },
        body_template: {
          order_id: "{{input.order_id}}",
          sender: "{{config.sender}}",
          attempt: "{{execution.attempt_count}}",
        },
        timeout_ms: 5000,
        expected_status_codes: [200],
      },
      retry_policy: { ...RETRY_FAST, initial_delay_ms: 1000 },
    },
    {
      // Points at the mock server's /flaky route: fails until the third call.
      name: "charge-webhook",
      type: "HTTP",
      config: "email-service",
      spec: {
        url: "{{config.api_base_url}}/flaky?succeed_after=3",
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body_template: { order_id: "{{input.order_id}}" },
        timeout_ms: 5000,
        expected_status_codes: [200],
      },
      retry_policy: RETRY_FAST,
    },
    {
      name: "minute-heartbeat",
      type: "HTTP",
      config: "email-service",
      spec: {
        url: "{{config.api_base_url}}/echo",
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body_template: { tick: "{{execution.execution_id}}" },
        timeout_ms: 5000,
        expected_status_codes: [200],
      },
      retry_policy: { ...RETRY_FAST, max_attempts: 1 },
    },
    {
      // Kafka and Redis Stream dispatches do not propagate the idempotency key
      // automatically the way HTTP does — it is templated in here by hand.
      name: "order-events-kafka",
      type: "KAFKA",
      spec: {
        bootstrap_servers: "localhost:9092",
        topic: "invokr-demo-orders",
        key_template: "{{input.order_id}}",
        value_template: { order_id: "{{input.order_id}}", source: "invokr-demo" },
        headers: { "idempotency-key": "{{execution.idempotency_key}}" },
        acks: "all",
        timeout_ms: 10000,
      },
      retry_policy: RETRY_FAST,
    },
    {
      name: "order-events-redis",
      type: "REDIS_STREAM",
      spec: {
        redis_url: "redis://127.0.0.1:6379",
        stream: "invokr-demo-orders",
        fields_template: {
          order_id: "{{input.order_id}}",
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

  // Create, or fall back to reading what is already there. Anything else throws
  // loudly — a half-provisioned demo is worse than one that refuses to start.
  async ensure(label, createFn, readFn) {
    const created = await createFn();
    if (created.ok) return created.body?.data ?? created.body;
    if (created.status === 409 || created.status === 200) {
      const existing = await readFn?.();
      if (existing?.ok) return existing.body?.data ?? existing.body;
      if (created.status === 200) return created.body?.data ?? created.body;
    }
    throw new Error(
      `${label}: ${created.status} ${JSON.stringify(created.body)?.slice(0, 300)}`,
    );
  }
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

  const result = { org_id: org.org_id, workspaces: {}, endpoints: [] };

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

    const scope = { org: org.org_id, workspace: row.workspace_id };

    await api.ensure(
      `payload-spec in ${ws.slug}`,
      () =>
        api.call("POST", "/v1/payload-specs", {
          ...scope,
          body: {
            name: "order-input",
            schema: {
              type: "object",
              properties: { order_id: { type: "string" }, user_id: { type: "string" } },
              required: ["order_id"],
            },
          },
        }),
      () => api.call("GET", "/v1/payload-specs/order-input", scope),
    );

    await api.ensure(
      `config in ${ws.slug}`,
      () =>
        api.call("POST", "/v1/configs", {
          ...scope,
          body: {
            name: "email-service",
            values: {
              api_base_url: mockUrl,
              sender: `noreply@${ws.slug}.invokr.internal`,
            },
          },
        }),
      () => api.call("GET", "/v1/configs/email-service", scope),
    );

    await api.ensure(
      `secret in ${ws.slug}`,
      () =>
        api.call("POST", "/v1/secrets", {
          ...scope,
          body: { name: "email_api_key", value: `sk-demo-${ws.slug}-9f2b41c7` },
        }),
      () => api.call("GET", "/v1/secrets/email_api_key", scope),
    );

    for (const ep of endpoints(mockUrl)) {
      const created = await api.call("POST", "/v1/endpoints", { ...scope, body: ep });
      if (!created.ok && created.status !== 409) {
        // A missing Kafka/Redis feature is a dispatch-time error, not a
        // registration error, so anything failing here is a real problem.
        throw new Error(
          `create endpoint ${ep.name} in ${ws.slug}: ${created.status} ${JSON.stringify(created.body)?.slice(0, 200)}`,
        );
      }
      if (ws.key === "a") result.endpoints.push({ name: ep.name, type: ep.type });
    }
  }

  return result;
}
