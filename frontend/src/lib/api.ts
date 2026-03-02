import { getToken, clearToken } from "./auth";

async function apiFetch(
  path: string,
  options: RequestInit = {}
): Promise<Response> {
  const token = getToken();
  const headers: Record<string, string> = {
    ...(options.headers as Record<string, string>),
  };
  if (token) {
    headers["Authorization"] = `Bearer ${token}`;
  }
  const res = await fetch(path, { ...options, headers });
  if (res.status === 401) {
    clearToken();
    window.location.reload();
  }
  return res;
}

export interface ChannelConfig {
  name: string;
  secretType: string;
  secretHeader: string | null;
  hasForward: boolean;
  forwardUrl: string | null;
  signHeader: string | null;
  expectedStatus: number | null;
  timeoutSeconds: number | null;
}

export interface AppConfigResponse {
  channels: ChannelConfig[];
}

export interface WebhookItem {
  id: number;
  headers: Record<string, string>;
  payload: unknown;
  received_at: number;
}

export interface TestSendResult {
  status: number;
  body: string;
}

export async function fetchConfig(): Promise<AppConfigResponse> {
  const res = await apiFetch("/api/config");
  if (!res.ok) throw new Error(`Failed to fetch config: ${res.status}`);
  return res.json();
}

export async function listWebhooks(channel: string): Promise<WebhookItem[]> {
  const res = await apiFetch(`/api/webhook/${channel}/list`);
  if (!res.ok) throw new Error(`Failed to list webhooks: ${res.status}`);
  return res.json();
}

export async function testSend(
  channel: string,
  payload: unknown,
  secret: string | null
): Promise<TestSendResult> {
  const res = await apiFetch(`/api/webhook/${channel}/test-send`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ secret: secret || null, payload }),
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text);
  }
  return res.json();
}

export async function deleteWebhook(
  channel: string,
  webhookId: number
): Promise<void> {
  const res = await apiFetch(`/api/webhook/${channel}/${webhookId}`, {
    method: "DELETE",
  });
  if (!res.ok) throw new Error(`Failed to delete webhook: ${res.status}`);
}
