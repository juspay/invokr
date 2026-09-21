// Provisions everything the demo scenes need, idempotently.
//
// Everything is upserted (create, else update in place) so a demo database that
// was provisioned by an older version of this file gets brought up to date
// instead of quietly keeping stale endpoint specs.
//
// The endpoints point at the mock server's business-shaped routes — an email
// service, a payment processor, a health sweep — because "Sent the welcome
// email to Priya" is a thing a room can picture, and "echoed your JSON" is not.

const ORG = { name: "Invokr Demo", slug: "invokr-demo" };

// Two workspaces, same endpoint name in both. Scene 7 fires into each and shows
// the rows landing in different schemas.
export const WORKSPACES = [
  { key: "a", name: "Payments", slug: "payments" },
  { key: "b", name: "Risk", slug: "risk" },
];

const RETRY = {
  max_attempts: 3,
  backoff: "exponential",
  initial_delay_ms: 2000,
  max_delay_ms: 30000,
};

const ORDER_INPUT_SCHEMA = {
  type: "object",
  properties: {
    order_id: { type: "string" },
    customer: { type: "string" },
    email: { type: "string" },
    user_id: { type: "string" },
  },
  required: ["order_id"],
};

function endpoints() {
  return [
    {
      name: "send-welcome-email",
      type: "HTTP",
      payload_spec: "order-input",
      config: "email-service",
      spec: {
        url: "{{config.api_base_url}}/emails/welcome",
        method: "POST",
        headers: {
          // Resolved from the encrypted secret store at execution time — the
          // demo shows that the API will not hand the value back.
          Authorization: "Bearer {{secret.email_api_key}}",
          "Content-Type": "application/json",
        },
        body_template: {
          customer: "{{input.customer}}",
          email: "{{input.email}}",
          order_id: "{{input.order_id}}",
          sent_by: "{{config.sender}}",
        },
        timeout_ms: 5000,
        expected_status_codes: [200],
      },
      retry_policy: { ...RETRY, initial_delay_ms: 1000 },
    },
    {
      // Fails twice before it succeeds, so the retry scene has something real
      // to retry.
      name: "charge-webhook",
      type: "HTTP",
      config: "email-service",
      spec: {
        url: "{{config.api_base_url}}/billing/charge?succeed_after=3",
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body_template: {
          order_id: "{{input.order_id}}",
          amount: "{{input.amount}}",
        },
        timeout_ms: 5000,
        expected_status_codes: [200],
      },
      retry_policy: RETRY,
    },
    {
      name: "minute-heartbeat",
      type: "HTTP",
      config: "email-service",
      spec: {
        url: "{{config.api_base_url}}/ops/heartbeat",
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body_template: { tick: "{{execution.execution_id}}" },
        timeout_ms: 5000,
        expected_status_codes: [200],
      },
      retry_policy: { ...RETRY, max_attempts: 1 },
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
        value_template: { order_id: "{{input.order_id}}", customer: "{{input.customer}}" },
        headers: { "idempotency-key": "{{execution.idempotency_key}}" },
        acks: "all",
        timeout_ms: 10000,
      },
      retry_policy: RETRY,
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
      retry_policy: RETRY,
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

  /// Create it, or update what's already there. Anything else throws loudly —
  /// a half-provisioned demo is worse than one that refuses to start.
  async upsert(label, collection, name, createBody, updateBody) {
    const created = await this.call("POST", `/v1/${collection}`, {
      ...this.scope,
      body: createBody,
    });
    if (created.ok) return created.body?.data ?? created.body;

    if (created.status === 409) {
      const updated = await this.call("PUT", `/v1/${collection}/${name}`, {
        ...this.scope,
        body: updateBody,
      });
      if (updated.ok) return updated.body?.data ?? updated.body;
      throw new Error(`${label}: update failed ${updated.status} ${JSON.stringify(updated.body)?.slice(0, 200)}`);
    }

    throw new Error(`${label}: ${created.status} ${JSON.stringify(created.body)?.slice(0, 250)}`);
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

    api.scope = { org: org.org_id, workspace: row.workspace_id };

    await api.upsert(
      `payload-spec in ${ws.slug}`,
      "payload-specs",
      "order-input",
      { name: "order-input", schema: ORDER_INPUT_SCHEMA },
      { schema: ORDER_INPUT_SCHEMA },
    );

    const values = {
      api_base_url: mockUrl,
      sender: `noreply@${ws.slug}.invokr.internal`,
    };
    await api.upsert(`config in ${ws.slug}`, "configs", "email-service", { name: "email-service", values }, { values });

    // Not a real key — a fixture, so that what the demo resolves and masks is
    // recognisably fake if it ever ends up on a projector.
    const value = `sk-demo-${ws.slug}-9f2b41c7`;
    await api.upsert(`secret in ${ws.slug}`, "secrets", "email_api_key", { name: "email_api_key", value }, { value });

    for (const ep of endpoints()) {
      const { name, ...rest } = ep;
      await api.upsert(`endpoint ${name} in ${ws.slug}`, "endpoints", name, ep, rest);
      if (ws.key === "a") result.endpoints.push({ name, type: ep.type });
    }
  }

  return result;
}
