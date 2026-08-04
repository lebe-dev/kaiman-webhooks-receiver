# FAQ

## 1. Does it secure?

Yes, check [SECURITY.md](SECURITY.md) for details.

## 2. How to configure?

Check [CONFIG.md](CONFIG.md).

## 3. Does it support multiple webhooks?

Yes, service supports multiple webhooks from different channels.

## 4. Does it provide REST API?

Yes, check [API.md](API.md).

## 5. What about CloudFlare Tunnel, ngrok, etc?

Check detail [comparison](COMPARISON.md).

## 6. How quickly are webhooks forwarded — in real-time?

Not exactly real-time, but close. Each channel with forwarding enabled gets a dedicated background loop that runs continuously:

- **If there are pending webhooks in the queue**, the loop forwards them back-to-back with no delay between each successful delivery — so a burst of webhooks is processed as fast as the target URL responds.
- **When nothing is due**, the loop sleeps for `interval-seconds` (configured per channel, e.g. 30 s) before checking again.
- **After a failed delivery** (network error or unexpected HTTP status), the loop sleeps for `interval-seconds`, and the webhook that failed is retried on an exponential delay: one `interval-seconds` after the first failure, then doubling up to an hour. Later webhooks in the queue are not held up by it. See [Retry backoff](CONFIG.md#retry-backoff).

In practice, the end-to-end latency is:

- **Best case**: near-zero — the webhook arrives while the loop is actively processing and is forwarded on the next iteration.
- **Worst case**: up to `interval-seconds` — the webhook arrives just after the loop went to sleep.

To reduce worst-case latency, lower `interval-seconds` in the channel's `forward` config. The default example value is 30 seconds.
