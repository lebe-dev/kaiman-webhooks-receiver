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
    globalThis.location.reload();
  }
  return res;
}

/** Retry schedule of a channel, with global defaults already resolved server side. */
export interface ForwardBackoffConfig {
  multiplier: number;
  maxSeconds: number;
  jitter: number;
}

export interface ChannelConfig {
  name: string;
  secretType: string;
  secretHeader: string | null;
  /** A secret is configured; its value never leaves the server. */
  hasSecret: boolean;
  hasForward: boolean;
  forwardUrl: string | null;
  signHeader: string | null;
  /** Forwards are signed with a dedicated sign-secret instead of the channel secret. */
  hasSignSecret: boolean;
  expectedStatus: number | null;
  timeoutSeconds: number | null;
  /** Pause between queue passes, and the delay before the first retry. */
  intervalSeconds: number | null;
  backoff: ForwardBackoffConfig | null;
  /** Effective body limit in bytes: the channel's own, or the global default. */
  maxBodySize: number;
  /** null means every source IP is accepted. */
  allowedIps: string[] | null;
  monitoringMetrics: boolean;
  note: string | null;
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

export interface ChannelForwardStatus {
  paused: boolean;
  queue_size: number;
  last_success_at: number | null;
  last_error_at: number | null;
  last_error_message: string | null;
  /** Soonest moment a queued webhook becomes due; null while one is due now. */
  next_attempt_at: number | null;
}

export interface QueueItem {
  id: number;
  headers: Record<string, string>;
  payload: unknown;
  received_at: number;
  forward_attempts: number;
  last_attempt_at: number | null;
  last_attempt_error: string | null;
  /** When the forwarder retries this webhook; null means it is due now. */
  next_attempt_at: number | null;
}

export interface QueueResponse {
  status: ChannelForwardStatus;
  items: QueueItem[];
}

export interface RetryResult {
  success: boolean;
  status_code: number | null;
  body: string | null;
  error: string | null;
}

export async function fetchQueue(channel: string): Promise<QueueResponse> {
  const res = await apiFetch(`/api/webhook/${channel}/queue`);
  if (!res.ok) throw new Error(`Failed to fetch queue: ${res.status}`);
  return res.json();
}

export async function pauseForwarding(channel: string): Promise<void> {
  const res = await apiFetch(`/api/webhook/${channel}/queue/pause`, {
    method: "POST",
  });
  if (!res.ok) throw new Error(`Failed to pause: ${res.status}`);
}

export async function resumeForwarding(channel: string): Promise<void> {
  const res = await apiFetch(`/api/webhook/${channel}/queue/resume`, {
    method: "POST",
  });
  if (!res.ok) throw new Error(`Failed to resume: ${res.status}`);
}

export async function clearQueue(channel: string): Promise<void> {
  const res = await apiFetch(`/api/webhook/${channel}/queue/clear`, {
    method: "POST",
  });
  if (!res.ok) throw new Error(`Failed to clear queue: ${res.status}`);
}

export async function retryWebhook(
  channel: string,
  webhookId: number
): Promise<RetryResult> {
  const res = await apiFetch(
    `/api/webhook/${channel}/queue/retry/${webhookId}`,
    { method: "POST" }
  );
  if (!res.ok) throw new Error(`Failed to retry: ${res.status}`);
  return res.json();
}
